use anyhow::{Context, Result};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::time::Duration;
use url::Url;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;

fn main() -> Result<()> {
    // Парсим аргументы командной строки
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 3 {
        eprintln!("Использование: {} <URL_сайта> <выходной_файл.xml>", args[0]);
        eprintln!("Пример: {} https://example.com sitemap.xml", args[0]);
        eprintln!("Пример с путём: {} https://example.com/news/ sitemap.xml", args[0]);
        std::process::exit(1);
    }

    let start_url = &args[1];
    let output_file = &args[2];

    // Парсим базовый URL
    let base_url = Url::parse(start_url)
    .context("Неверный формат URL")?;

    let base_domain = base_url.host_str()
    .context("URL не содержит домен")?;

    // Сохраняем путь для фильтрации
    let base_path = base_url.path().to_string();

    println!("🔍 Начинаем сканирование сайта: {}", start_url);
    println!("📋 Базовый домен: {}", base_domain);

    if base_path != "/" && !base_path.is_empty() {
        println!("🎯 Фильтрация по пути: {}", base_path);
    } else {
        println!("🌐 Сканирование всего домена");
    }

    // Создаем HTTP клиент
    let client = Client::builder()
    .timeout(Duration::from_secs(30))
    .user_agent("SitemapGenerator/1.0")
    .build()?;

    // Множества для отслеживания URL
    let mut visited_urls = HashSet::new();
    let mut discovered_urls = HashSet::new();
    let mut queue = VecDeque::new();

    // Добавляем начальный URL
    let normalized_start = normalize_url(&base_url, start_url);
    queue.push_back(normalized_start.clone());
    discovered_urls.insert(normalized_start);

    // Начинаем обход
    let mut page_count = 0;

    while let Some(current_url) = queue.pop_front() {
        if visited_urls.contains(&current_url) {
            continue;
        }

        println!("📄 Сканируем: {} (осталось в очереди: {})", current_url, queue.len());

        match fetch_and_parse(&client, &current_url, &base_url, base_domain, &base_path) {
            Ok((urls, status)) => {
                visited_urls.insert(current_url);

                if status == 200 {
                    page_count += 1;

                    for url in urls {
                        if !discovered_urls.contains(&url) {
                            discovered_urls.insert(url.clone());
                            queue.push_back(url);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("⚠️  Ошибка при обработке {}: {}", current_url, e);
                // Всё равно добавляем в посещённые, чтобы не зациклиться
                visited_urls.insert(current_url);
            }
        }

        // Небольшая задержка, чтобы не нагружать сервер
        std::thread::sleep(Duration::from_millis(100));
    }

    println!("✅ Сканирование завершено. Найдено {} страниц.", page_count);

    // Генерируем карту сайта
    generate_sitemap(&visited_urls, output_file)?;

    println!("✅ Карта сайта сохранена в файл: {}", output_file);

    Ok(())
}

fn fetch_and_parse(
    client: &Client,
    url: &str,
    base_url: &Url,
    base_domain: &str,
    base_path: &str,
) -> Result<(Vec<String>, u16)> {
    // Отправляем GET запрос
    let response = client.get(url).send()?;
    let status = response.status().as_u16();

    if status != 200 {
        return Ok((Vec::new(), status));
    }

    // Проверяем Content-Type
    let content_type = response.headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("");

    if !content_type.contains("text/html") && !content_type.contains("application/xhtml") {
        return Ok((Vec::new(), status));
    }

    // Получаем HTML
    let body = response.text()?;

    // Парсим HTML и извлекаем ссылки
    let urls = extract_links(&body, url, base_url, base_domain, base_path);

    Ok((urls, status))
}

fn extract_links(html: &str, current_url: &str, _base_url: &Url, base_domain: &str, base_path: &str) -> Vec<String> {
    let mut urls = Vec::new();

    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();

    for element in document.select(&selector) {
        if let Some(href) = element.value().attr("href") {
            // Пропускаем якоря и javascript
            if href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") || href.starts_with("tel:") {
                continue;
            }

            // Разрешаем относительные URL
            if let Ok(absolute_url) = resolve_url(href, current_url) {
                // Проверяем, что это внутренняя ссылка и она соответствует фильтру
                if let Ok(parsed) = Url::parse(&absolute_url) {
                    // Проверяем домен
                    if parsed.host_str() == Some(base_domain) {
                        // Проверяем путь, если задан фильтр
                        let path = parsed.path();
                        let should_include = if base_path.ends_with('/') {
                            // Если базовый путь заканчивается на '/', ищем все подпути
                            path.starts_with(base_path) || path == base_path.trim_end_matches('/')
                        } else {
                            // Если базовый путь без '/' в конце, проверяем точное совпадение или подпути
                            path == base_path || path.starts_with(&format!("{}/", base_path))
                        };

                        if should_include {
                            // Убираем фрагмент и нормализуем
                            let mut normalized = parsed.clone();
                            normalized.set_fragment(None);

                            // Убираем стандартные порты
                            if (normalized.scheme() == "http" && normalized.port() == Some(80)) ||
                                (normalized.scheme() == "https" && normalized.port() == Some(443)) {
                                    normalized.set_port(None).ok();
                                }

                                let url_str = normalized.to_string();

                            // Пропускаем не-HTTP(S) схемы
                            if normalized.scheme() == "http" || normalized.scheme() == "https" {
                                urls.push(url_str);
                            }
                        }
                    }
                }
            }
        }
    }

    urls
}

fn resolve_url(href: &str, current_url: &str) -> Result<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        Ok(href.to_string())
    } else {
        // Относительный URL
        let current = Url::parse(current_url)?;
        let resolved = current.join(href)?;
        Ok(resolved.to_string())
    }
}

fn normalize_url(base_url: &Url, url_str: &str) -> String {
    if let Ok(parsed) = Url::parse(url_str) {
        let mut normalized = parsed;
        normalized.set_fragment(None);

        if (normalized.scheme() == "http" && normalized.port() == Some(80)) ||
            (normalized.scheme() == "https" && normalized.port() == Some(443)) {
                normalized.set_port(None).ok();
            }

            // Убираем слеш в конце для нормализации
            let mut result = normalized.to_string();
        if result.ends_with('/') {
            result.pop();
        }

        result
    } else if let Ok(resolved) = base_url.join(url_str) {
        let mut normalized = resolved;
        normalized.set_fragment(None);

        let mut result = normalized.to_string();
        if result.ends_with('/') {
            result.pop();
        }

        result
    } else {
        url_str.to_string()
    }
}

fn generate_sitemap(urls: &HashSet<String>, output_file: &str) -> Result<()> {
    let file = File::create(output_file)?;
    let mut writer = Writer::new_with_indent(&file, b' ', 2);

    // XML декларация
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    // Корневой элемент с атрибутом
    let mut urlset_start = BytesStart::new("urlset");
    urlset_start.push_attribute(("xmlns", "http://www.sitemaps.org/schemas/sitemap/0.9"));
    writer.write_event(Event::Start(urlset_start))?;

    // Добавляем URL'ы
    for url in urls {
        writer.write_event(Event::Start(BytesStart::new("url")))?;

        // Элемент loc
        writer.write_event(Event::Start(BytesStart::new("loc")))?;
        writer.write_event(Event::Text(BytesText::new(url)))?;
        writer.write_event(Event::End(BytesEnd::new("loc")))?;

        writer.write_event(Event::End(BytesEnd::new("url")))?;
    }

    writer.write_event(Event::End(BytesEnd::new("urlset")))?;

    Ok(())
}
