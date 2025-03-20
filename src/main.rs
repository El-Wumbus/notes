#![feature(path_file_prefix)]

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use log::{error, info, warn};
use rinja::Template;
use serde::{Deserialize, Serialize};
use signal_hook::consts::SIGHUP;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server};

#[allow(dead_code)]
mod uri;

const STYLES: &str = include_str!("styles.css");

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Config {
    #[serde(default = "Config::default_content_path")]
    content_path: PathBuf,
    #[serde(default = "Config::default_bind")]
    bind:         std::net::SocketAddr,
}

impl Config {
    fn default_content_path() -> PathBuf {
        PathBuf::from(".")
    }
    fn default_bind() -> std::net::SocketAddr {
        "127.0.0.1:3000".parse().unwrap()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            content_path: Self::default_content_path(),
            bind:         Self::default_bind(),
        }
    }
}

#[derive(Debug, Clone)]
struct IndexedDocument {
    title:    String,
    created:  NaiveDate,
    rel_path: String,
}
type Index = Vec<IndexedDocument>;

fn main() {
    use log::LevelFilter;
    env_logger::Builder::new()
        .filter(None, LevelFilter::Debug)
        .init();
    let reload_state = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGHUP, reload_state.clone()).unwrap();

    let config_path = dirs::config_dir()
        .expect("config directory")
        .join("notes/notes.toml");
    let mut config = load_config(&config_path);

    let content_path = fs::canonicalize(config.content_path).unwrap();
    let state = match SrvState::load(content_path) {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(e) => {
            error!("Failed to load state: {e}");
            std::process::exit(1);
        }
    };

    std::thread::spawn({
        let state = Arc::clone(&state);
        move || match Server::http(config.bind) {
            Ok(server) => SrvState::serve(state, server),
            Err(e) => {
                error!("Failed to bind server to {}: {}", config.bind, e);
                std::process::exit(1);
            }
        }
    });

    loop {
        config = load_config(&config_path);
        if reload_state.swap(false, Ordering::Relaxed) {
            info!("Reloading state...");
            let Ok(mut state) = state.lock() else { break };
            match SrvState::load(config.content_path) {
                Ok(s) => {
                    info!("State reloaded sucessfully!");
                    *state = s;
                }
                Err(e) => {
                    error!("Failed to reload state (retaining previous state): {e}")
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(256));
    }
}

fn load_config(config_path: impl AsRef<Path>) -> Config {
    let config_path = config_path.as_ref();
    let config_dir = config_path
        .parent()
        .expect("this is a file with a parent dir");
    let mut config = Config::default();
    if config_path.exists() {
        let contents = match std::fs::read_to_string(config_path) {
            Ok(contents) => contents,
            Err(e) => {
                error!("Failed to read config file \"{config_path:?}\": {e}");
                warn!("Using default config");
                return config;
            }
        };
        match toml::de::from_str(&contents) {
            Ok(x) => config = x,
            Err(e) => {
                error!("Failed to parse config: {e}");
                warn!("Using default config");
            }
        }
    } else {
        info!("Using default config");
        if !config_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(config_dir) {
                error!(
                    "Couldn't create parent directory \"{config_dir:?}\" for new config file \"{config_path:?}\": {e}"
                );
                return config;
            }
        }
        let contents = toml::ser::to_string(&config).unwrap();
        match fs::File::create(config_path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(contents.as_bytes()) {
                    error!("Failed to write default config to \"{config_path:?}\": {e}");
                }
            }
            Err(e) => error!("Failed to create config file \"{config_path:?}\": {e}"),
        }
    }
    config
}

#[derive(Default)]
struct SrvState {
    content_path: PathBuf,
    index:        Index,
    index_html:   String,
}

impl SrvState {
    fn load(content_path: PathBuf) -> io::Result<Self> {
        let index = generate_index(&content_path)?;
        if index.is_empty() {
            warn!("Index is empty!");
        }
        let (index_html, _) = mdtodoc(
            &generate_index_html(&index),
            Meta {
                title: String::from("Index"),
                date:  NaiveDate::default().into(),
                lang:  None,
                desc:  None,
            },
        );
        Ok(Self {
            content_path,
            index,
            index_html,
        })
    }

    fn serve(state: Arc<Mutex<Self>>, server: Server) {
        loop {
            let request = match server.recv() {
                Ok(rq) => rq,
                Err(e) => {
                    error!("{e}");
                    break;
                }
            };

            let state = state.lock().unwrap();

            let method = request.method();
            let Some(path) = uri::percent_decode(request.url()) else {
                respond_or_log(request, Response::empty(400));
                continue;
            };

            match (path.as_str(), method) {
                ("/", Method::Get) => respond_or_log(
                    request,
                    Response::from_string(&state.index_html).with_header(
                        Header::from_bytes(b"Content-Type", b"text/html").unwrap(),
                    ),
                ),
                _ if path.starts_with("/note/") => {
                    let path = path.strip_prefix("/note/").unwrap();
                    let Some(entry) =
                        state.index.iter().find(|entry| entry.rel_path == path)
                    else {
                        respond_or_log(request, Response::empty(404));
                        continue;
                    };
                    let data_path = state.content_path.join(entry.rel_path.as_str());
                    let data = std::fs::read_to_string(&data_path).unwrap();
                    let (document, _) = mdtodoc(
                        &data,
                        Meta::inferred(entry.title.clone(), entry.created),
                    );
                    respond_or_log(
                        request,
                        Response::from_string(document).with_header(
                            Header::from_bytes(b"Content-Type", b"text/html").unwrap(),
                        ),
                    )
                }
                _ => {
                    respond_or_log(request, Response::empty(404));
                }
            }
            std::mem::drop(state);
        }
    }
}

fn respond_or_log<R: io::Read>(request: Request, response: Response<R>) {
    if let Err(e) = request.respond(response) {
        error!("Failed to respond to request: {e}");
    }
}

fn generate_index(content_path: &Path) -> std::io::Result<Index> {
    let mut index = Vec::new();
    let mut contents = String::new();
    walk(content_path, &mut |is_dir, path| {
        if path
            .file_name()
            .map(|x| x.as_encoded_bytes())
            .is_some_and(|x| x.starts_with(b"."))
        {
            return Ok(false);
        }
        if !is_dir {
            let guess = mime_guess::from_path(path).first();
            if guess.is_none_or(|guess| guess != "text/markdown") {
                return Ok(true);
            }
            let metadata = fs::metadata(path)?;
            let created = DateTime::<chrono::offset::Local>::from(
                metadata
                    .created()
                    .or(metadata.modified())
                    .unwrap_or_else(|_| std::time::SystemTime::now()),
            )
            .date_naive();
            let title = match Path::new(path.file_name().expect("not a dir"))
                .file_prefix()
                .and_then(|x| x.to_str())
            {
                Some(t) => t.to_string(),
                None => {
                    warn!(
                        "Invalid document title found in \"{path:?}\". Will attempt to find a title in its metadata..."
                    );
                    String::from("INVALID")
                }
            };

            let mut f = fs::File::open(path)?;
            f.read_to_string(&mut contents)?;
            let (_, meta) = mdtodoc(&contents, Meta::inferred(title, created));
            contents.clear();
            let Some(rel_path) = path
                .strip_prefix(content_path)
                .ok()
                .and_then(Path::to_str)
                .map(str::to_string)
            else {
                error!("Skipping document due to invalid path: \"{path:?}\"");
                return Ok(true);
            };

            index.push(IndexedDocument {
                title: meta.title,
                created: meta.date.into(),
                rel_path,
            });
        }
        Ok(true)
    })?;
    index.sort_by(|left, right| right.created.cmp(&left.created));
    Ok(index)
}

fn generate_index_html(index: &[IndexedDocument]) -> String {
    let mut page = String::new();
    page.push_str(r#"<ol style="list-style-type: none">"#);
    for doc in index {
        page.push_str(&format!(
            r#"<li> <time datetime="{time}">{time}</time> - <a href="/note/{path}">{title}</a></li>"#,
            time = doc.created, path = doc.rel_path, title = doc.title
        ));
    }
    page.push_str(r#"</ol>"#);
    page
}

#[derive(Debug, Clone, Deserialize)]
struct Meta {
    title: String,
    date:  NaiveDateTime,
    lang:  Option<String>,
    desc:  Option<String>,
}

impl Meta {
    fn inferred(title: String, created: NaiveDate) -> Self {
        Self {
            title,
            date: NaiveDateTime::from(created),
            lang: None,
            desc: None,
        }
    }
}

#[derive(Template)]
#[template(
    ext = "html",
    escape = "none",
    source = r#"
        <!DOCTYPE html>
        {% match meta.lang %}
            {% when Some with (lang) %} <html lang="{{ lang }}">
            {% when None %} <html lang="en-US">
        {% endmatch %}
        <head>
            <meta charset="utf-8" />
            <title>{{ meta.title|e("html") }}</title>
            <meta property="og:title" content="{{ meta.title|e("html") }}" />

            {% match meta.desc %}
                {% when Some with (desc) %}
                    <meta name="description" content="{{ desc|e("html") }}" />
                    <meta property="og:description" content="{{ desc|e("html") }}" />
                {% when None %}
            {% endmatch %}
            <style> {{ styles }} </style>
        </head>
        <body><main>
        <h1> {{ meta.title|e("html") }}</h1>
        <article>{{ markdown }}</article>
        </main></body>
        </html>
        "#
)]
struct DocumentTemplate<'a> {
    meta:     Meta,
    styles:   &'a str,
    markdown: &'a str,
}

fn mdtodoc(md: &str, infered_meta: Meta) -> (String, Meta) {
    use std::collections::HashMap;
    use std::fmt::Write as _;

    use pulldown_cmark::{
        CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd, html,
    };

    use std::sync::LazyLock;
    use syntect::highlighting::{Theme, ThemeSet};
    use syntect::parsing::SyntaxSet;
    static SYNTAX_SET: LazyLock<SyntaxSet> =
        LazyLock::new(SyntaxSet::load_defaults_newlines);
    static THEME: LazyLock<Theme> = LazyLock::new(|| {
        let theme_set = ThemeSet::load_defaults();
        theme_set.themes["base16-ocean.dark"].clone()
    });

    #[derive(Default)]
    enum ParseState {
        #[default]
        Normal,
        Meta,
        Highlight,
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_MATH);

    let mut state = ParseState::default();
    let mut code = String::new();
    let mut meta = None;
    let mut syntax = SYNTAX_SET.find_syntax_plain_text();

    // To generate this style, you have to collect the footnotes at the end, while
    // parsing. You also need to count usages.
    let mut footnotes = Vec::new();
    let mut in_footnote = Vec::new();
    let mut footnote_numbers = HashMap::new();
    let parser = Parser::new_ext(md, options)
        .filter_map(|event| {
            match event {
                Event::Start(Tag::FootnoteDefinition(_)) => {
                    in_footnote.push(vec![event]);
                    None
                }
                Event::End(TagEnd::FootnoteDefinition) => {
                    let mut f = in_footnote.pop().unwrap();
                    f.push(event);
                    footnotes.push(f);
                    None
                }
                Event::FootnoteReference(name) => {
                    let n = footnote_numbers.len() + 1;
                    let (n, nr) = footnote_numbers.entry(name.clone()).or_insert((n, 0usize));
                    *nr += 1;
                    let html = Event::Html(format!(r##"<sup class="footnote-reference" id="fr-{name}-{nr}"><a href="#fn-{name}">[{n}]</a></sup>"##).into());
                    if in_footnote.is_empty() {
                        Some(html)
                    } else {
                        in_footnote.last_mut().unwrap().push(html);
                        None
                    }
                }
                _ if !in_footnote.is_empty() => {
                    in_footnote.last_mut().unwrap().push(event);
                    None
                }
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) => {
                    let lang = lang.trim();
                    if lang == "meta" {
                        state = ParseState::Meta;
                        None
                    } else {
                        state = ParseState::Highlight;
                        syntax = SYNTAX_SET
                            .find_syntax_by_token(lang)
                            .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
                        None
                    }
                }
                Event::Text(text) => match state {
                    ParseState::Normal => Some(Event::Text(text)),
                    ParseState::Meta => {
                        match toml::de::from_str::<Meta>(&text) {
                            Ok(m) => meta = Some(m),
                            Err(e) => error!("Failed to parse metadata: {e}"),
                        }
                        None
                    }
                    ParseState::Highlight => {
                        code.push_str(&text);
                        None
                    }
                },
                Event::End(TagEnd::CodeBlock) => match state {
                    ParseState::Normal => Some(Event::End(TagEnd::CodeBlock)),
                    ParseState::Meta => {
                        state = ParseState::Normal;
                        None
                    }
                    ParseState::Highlight => {
                        let html = syntect::html::highlighted_html_for_string(
                            &code,
                            &SYNTAX_SET,
                            syntax,
                            &THEME,
                        )
                        .unwrap_or(code.clone());
                        code.clear();
                        state = ParseState::Normal;
                        Some(Event::Html(html.into()))
                    }
                },
                _ => Some(event),
            }
        });

    let mut output = String::new();
    html::write_html_fmt(&mut output, parser).unwrap();

    // To make the footnotes look right, we need to sort them by their appearance
    // order, not by the in-tree order of their actual definitions. Unused items
    // are omitted entirely.
    //
    // For example, this code:
    //
    //     test [^1] [^2]
    //     [^2]: second used, first defined
    //     [^1]: test
    //
    // Gets rendered like *this* if you copy it into a GitHub comment box:
    //
    //     <p>test <sup>[1]</sup> <sup>[2]</sup></p>
    //     <hr>
    //     <ol>
    //     <li>test ↩</li>
    //     <li>second used, first defined ↩</li>
    //     </ol>
    if !footnotes.is_empty() {
        footnotes.retain(|f| match f.first() {
            Some(Event::Start(Tag::FootnoteDefinition(name))) => {
                footnote_numbers.get(name).unwrap_or(&(0, 0)).1 != 0
            }
            _ => false,
        });
        footnotes.sort_by_cached_key(|f| match f.first() {
            Some(Event::Start(Tag::FootnoteDefinition(name))) => {
                footnote_numbers.get(name).unwrap_or(&(0, 0)).0
            }
            _ => unreachable!(),
        });
        output.push_str("<hr><ol class=\"footnotes-list\">\n");
        html::write_html_fmt(
            &mut output,
            footnotes.into_iter().flat_map(|fl| {
                // To write backrefs, the name needs kept until the end of the footnote
                // definition.
                let mut name = CowStr::from("");
                // Backrefs are included in the final paragraph of the footnote, if it's
                // normal text. For example, this DOM can be produced:
                //
                // Markdown:
                //
                //     five [^feet].
                //
                //     [^feet]:
                //         A foot is defined, in this case, as 0.3048 m.
                //
                //         Historically, the foot has not been defined this way,
                // corresponding to many         subtly different units
                // depending on the location.
                //
                // HTML:
                //
                //     <p>five <sup class="footnote-reference" id="fr-feet-1"><a
                // href="#fn-feet">[1]</a></sup>.</p>
                //
                //     <ol class="footnotes-list">
                //     <li id="fn-feet">
                //     <p>A foot is defined, in this case, as 0.3048 m.</p>
                //     <p>Historically, the foot has not been defined this way,
                // corresponding to many     subtly different units
                // depending on the location. <a href="#fr-feet-1">↩</a></p>
                //     </li>
                //     </ol>
                //
                // This is mostly a visual hack, so that footnotes use less vertical
                // space.
                //
                // If there is no final paragraph, such as a tabular, list, or image
                // footnote, it gets pushed after the last tag instead.
                let mut has_written_backrefs = false;
                let fl_len = fl.len();
                let footnote_numbers = &footnote_numbers;
                fl.into_iter().enumerate().map(move |(i, f)| match f {
                    Event::Start(Tag::FootnoteDefinition(current_name)) => {
                        name = current_name;
                        has_written_backrefs = false;
                        Event::Html(format!(r##"<li id="fn-{name}">"##).into())
                    }
                    Event::End(TagEnd::FootnoteDefinition)
                    | Event::End(TagEnd::Paragraph)
                        if !has_written_backrefs && i >= fl_len - 2 =>
                    {
                        let usage_count = footnote_numbers.get(&name).unwrap().1;
                        let mut end = String::with_capacity(
                            name.len()
                                + (r##" <a href="#fr--1">↩</a></li>"##.len()
                                    * usage_count),
                        );
                        for usage in 1..=usage_count {
                            if usage == 1 {
                                write!(
                                    &mut end,
                                    r##" <a href="#fr-{name}-{usage}">↩</a>"##
                                )
                                .unwrap();
                            } else {
                                write!(
                                    &mut end,
                                    r##" <a href="#fr-{name}-{usage}">↩{usage}</a>"##
                                )
                                .unwrap();
                            }
                        }
                        has_written_backrefs = true;
                        if f == Event::End(TagEnd::FootnoteDefinition) {
                            end.push_str("</li>\n");
                        } else {
                            end.push_str("</p>\n");
                        }
                        Event::Html(end.into())
                    }
                    Event::End(TagEnd::FootnoteDefinition) => {
                        Event::Html("</li>\n".into())
                    }
                    Event::FootnoteReference(_) => {
                        unreachable!("converted to HTML earlier")
                    }
                    f => f,
                })
            }),
        )
        .unwrap();
        output.push_str("</ol>\n");
    }
    let meta = meta.unwrap_or(infered_meta);
    let template = DocumentTemplate {
        styles:   STYLES,
        meta:     meta.clone(),
        markdown: &output,
    };
    let html = template.render().unwrap();
    (html, meta)
}

fn walk<F: FnMut(bool, &Path) -> std::io::Result<bool>>(
    p: impl AsRef<std::path::Path>,
    callback: &mut F,
) -> Result<(), std::io::Error> {
    let dir = p.as_ref();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if callback(true, &path)? {
                    walk(path, callback)?;
                }
            } else {
                callback(false, &path)?;
            }
        }
    } else {
        // We don't want to ignore the first item if it's a file
        callback(false, dir)?;
    }
    Ok(())
}
