#![feature(path_file_prefix)]

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use clap::Parser;
use log::{error, info, warn};
use rinja::Template;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{
    env, fs,
    io::{self, Write},
};
use tiny_http::{Header, Method, Request, Response, Server};

#[allow(dead_code)]
mod uri;
const STYLES: &str = include_str!("styles.css");

// TODO: Config & content reloading

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Config {
    #[serde(default = "Config::default_content_path")]
    content_path: PathBuf,
    #[serde(default = "Config::default_bind")]
    bind: std::net::SocketAddr,
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
            bind: Self::default_bind(),
        }
    }
}

#[derive(Debug, Clone)]
struct IndexedDocument {
    title: String,
    created: NaiveDate,
    rel_path: String,
}
type Index = Vec<IndexedDocument>;

fn main() {
    use log::LevelFilter;
    env_logger::Builder::new()
        .filter(None, LevelFilter::Debug)
        .init();

    let home = PathBuf::from(env::var_os("HOME").unwrap());
    let config_dir = home.join(".config/notes");
    let config_path = config_dir.join("notes.toml");
    let config = Config::default();
    let config = if config_path.exists() {
        let contents = std::fs::read_to_string(&config_path).unwrap();
        let config: Config = match toml::de::from_str(&contents) {
            Ok(a) => a,
            Err(e) => {
                error!("Config: {e}");
                config
            }
        };
        config
    } else {
        if !config_dir.exists() {
            std::fs::create_dir_all(config_dir).unwrap();
        }
        let mut config_f = fs::File::create(&config_path).unwrap();
        let contents = toml::ser::to_string(&config).unwrap();
        config_f.write_all(contents.as_bytes()).unwrap();
        config
    };

    let content_path = fs::canonicalize(config.content_path).unwrap();
    let state = SrvState::load(content_path).unwrap();
    state.serve(Server::http(config.bind).unwrap());
}

#[derive(Default)]
struct SrvState {
    content_path: PathBuf,
    index: Index,
    index_html: String,
}

impl SrvState {
    fn load(content_path: PathBuf) -> io::Result<Self> {
        let index = generate_index(&content_path)?;
        if index.is_empty() {
            warn!("Index is empty!");
        }
        let index_html = markdown_to_document(&generate_index_html(dbg!(&index)), Meta {
            title: String::from("Index"),
            date: NaiveDate::default().into(),
            lang: None,
            desc: None,
        });
        Ok(Self {
            content_path,
            index,
            index_html,
        })
    }

    fn serve(&self, server: Server) {
        loop {
            // blocks until the next request is received
            let request = match server.recv() {
                Ok(rq) => rq,
                Err(e) => {
                    error!("{e}");
                    break;
                }
            };

            let path = uri::percent_decode(request.url()).unwrap();
            let method = request.method();
            info!("Got {method} {path}");
            match (path.as_str(), method) {
                ("/", Method::Get) => respond_or_log(
                    request,
                    Response::from_string(&self.index_html).with_header(
                        Header::from_bytes(b"Content-Type", b"text/html").unwrap(),
                    ),
                ),
                _ if path.starts_with("/note/") => {
                    let path = path.strip_prefix("/note/").unwrap();
                    let Some(entry) =
                        self.index.iter().find(|entry| entry.rel_path == path)
                    else {
                        warn!("Couldn't find requested note: '{path}'");
                        respond_or_log(request, Response::empty(404));
                        continue;
                    };
                    let data_path = self.content_path.join(entry.rel_path.as_str());
                    let data = std::fs::read_to_string(&data_path).unwrap();
                    let document = markdown_to_document(
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
        }
    }
}

fn respond_or_log<R: io::Read>(request: Request, response: Response<R>) {
    if let Err(e) = request.respond(response) {
        error!("Failed to respond to request: {e}");
    }
}

// TODO: Error handling -- no more unwrapping in the wwalk!!!
fn generate_index(content_path: &Path) -> std::io::Result<Index> {
    let mut index = Vec::new();
    walk(content_path, &mut |is_dir, path| {
        if !is_dir {
            let guess = mime_guess::from_path(path).first();
            if guess.is_none_or(|guess| guess != "text/markdown") {
                return Ok(true);
            }
            let metadata = fs::metadata(path)?;
            let created =
                DateTime::<chrono::offset::Local>::from(metadata.created()?).date_naive();
            let title = Path::new(path.file_name().unwrap()).file_prefix().unwrap();

            let rel_path = path.strip_prefix(content_path).unwrap();
            index.push(IndexedDocument {
                title: title.to_str().unwrap().to_string(),
                created,
                rel_path: rel_path.to_str().unwrap().to_string(),
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
            r#"<li> {} - <a href="/note/{}">{}</a></li>"#,
            doc.created, doc.rel_path, doc.title
        ));
    }
    page.push_str(r#"</ol>"#);
    page
}

#[derive(Debug, Clone, Deserialize)]
struct Meta {
    title: String,
    date: NaiveDateTime,
    lang: Option<String>,
    desc: Option<String>,
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
        <html>
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
        <body>
        {{ markdown }}
        </body>
        </html>
        "#
)]
struct DocumentTemplate<'a> {
    meta: Meta,
    styles: &'a str,
    markdown: &'a str,
}

fn markdown_to_document(contents: &str, infered_meta: Meta) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
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

    let mut state = ParseState::default();
    let mut code = String::new();
    let mut meta = None;
    let mut syntax = SYNTAX_SET.find_syntax_plain_text();
    let parser = Parser::new_ext(contents, options).filter_map(|event| match event {
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
    });

    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);

    let template = DocumentTemplate {
        styles: STYLES,
        meta: meta.clone().unwrap_or(infered_meta),
        markdown: &html_output,
    };
    let html = template.render().unwrap();
    html
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
