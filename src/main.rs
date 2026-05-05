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
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 3 {
        eprintln!("Usage: {} <website_URL> <output_file.xml>", args[0]);
        eprintln!("Example: {} https://example.com sitemap.xml", args[0]);
        eprintln!("Example with path: {} https://example.com/news/ sitemap.xml", args[0]);
        std::process::exit(1);
    }

    let start_url = &args[1];
    let output_file = &args[2];

    // Parse the base URL
    let base_url = Url::parse(start_url)
    .context("Invalid URL format")?;

    let base_domain = base_url.host_str()
    .context("URL does not contain a domain")?;

    // Save the path for filtering
    let base_path = base_url.path().to_string();

    println!("🔍 Starting website scan: {}", start_url);
    println!("📋 Base domain: {}", base_domain);

    if base_path != "/" && !base_path.is_empty() {
        println!("🎯 Path filtering: {}", base_path);
    } else {
        println!("🌐 Scanning entire domain");
    }

    // Create HTTP client
    let client = Client::builder()
    .timeout(Duration::from_secs(30))
    .user_agent("SitemapGenerator/1.0")
    .build()?;

    // Sets for tracking URLs
    let mut visited_urls = HashSet::new();
    let mut discovered_urls = HashSet::new();
    let mut queue = VecDeque::new();

    // Add the starting URL
    let normalized_start = normalize_url(&base_url, start_url);
    queue.push_back(normalized_start.clone());
    discovered_urls.insert(normalized_start);

    // Start crawling
    let mut page_count = 0;

    while let Some(current_url) = queue.pop_front() {
        if visited_urls.contains(&current_url) {
            continue;
        }

        println!("📄 Scanning: {} (remaining in queue: {})", current_url, queue.len());

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
                eprintln!("⚠️  Error processing {}: {}", current_url, e);
                // Still add to visited to avoid infinite loops
                visited_urls.insert(current_url);
            }
        }

        // Small delay to avoid overloading the server
        std::thread::sleep(Duration::from_millis(100));
    }

    println!("✅ Scan completed. Found {} pages.", page_count);

    // Generate the sitemap
    generate_sitemap(&visited_urls, output_file)?;

    println!("✅ Sitemap saved to file: {}", output_file);

    Ok(())
}

fn fetch_and_parse(
    client: &Client,
    url: &str,
    base_url: &Url,
    base_domain: &str,
    base_path: &str,
) -> Result<(Vec<String>, u16)> {
    // Send GET request
    let response = client.get(url).send()?;
    let status = response.status().as_u16();

    if status != 200 {
        return Ok((Vec::new(), status));
    }

    // Check Content-Type
    let content_type = response.headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("");

    if !content_type.contains("text/html") && !content_type.contains("application/xhtml") {
        return Ok((Vec::new(), status));
    }

    // Get HTML content
    let body = response.text()?;

    // Parse HTML and extract links
    let urls = extract_links(&body, url, base_url, base_domain, base_path);

    Ok((urls, status))
}

fn extract_links(html: &str, current_url: &str, _base_url: &Url, base_domain: &str, base_path: &str) -> Vec<String> {
    let mut urls = Vec::new();

    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();

    for element in document.select(&selector) {
        if let Some(href) = element.value().attr("href") {
            // Skip anchors and javascript links
            if href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") || href.starts_with("tel:") {
                continue;
            }

            // Resolve relative URLs
            if let Ok(absolute_url) = resolve_url(href, current_url) {
                // Check if it's an internal link matching the filter
                if let Ok(parsed) = Url::parse(&absolute_url) {
                    // Check domain
                    if parsed.host_str() == Some(base_domain) {
                        // Check path if filter is set
                        let path = parsed.path();
                        let should_include = if base_path.ends_with('/') {
                            // If base path ends with '/', find all subpaths
                            path.starts_with(base_path) || path == base_path.trim_end_matches('/')
                        } else {
                            // If base path without '/' at the end, check exact match or subpaths
                            path == base_path || path.starts_with(&format!("{}/", base_path))
                        };

                        if should_include {
                            // Remove fragment and normalize
                            let mut normalized = parsed.clone();
                            normalized.set_fragment(None);

                            // Remove default ports
                            if (normalized.scheme() == "http" && normalized.port() == Some(80)) ||
                                (normalized.scheme() == "https" && normalized.port() == Some(443)) {
                                    normalized.set_port(None).ok();
                                }

                                let url_str = normalized.to_string();

                            // Skip non-HTTP(S) schemes
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
        // Relative URL
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

            // Remove trailing slash for normalization
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

    // XML declaration
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    // Root element with attribute
    let mut urlset_start = BytesStart::new("urlset");
    urlset_start.push_attribute(("xmlns", "http://www.sitemaps.org/schemas/sitemap/0.9"));
    writer.write_event(Event::Start(urlset_start))?;

    // Add URLs
    for url in urls {
        writer.write_event(Event::Start(BytesStart::new("url")))?;

        // loc element
        writer.write_event(Event::Start(BytesStart::new("loc")))?;
        writer.write_event(Event::Text(BytesText::new(url)))?;
        writer.write_event(Event::End(BytesEnd::new("loc")))?;

        writer.write_event(Event::End(BytesEnd::new("url")))?;
    }

    writer.write_event(Event::End(BytesEnd::new("urlset")))?;

    Ok(())
}
