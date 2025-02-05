#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("URI failed to validate")]
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uri<'a> {
    pub scheme: Option<&'a str>,
    pub userinfo: Option<&'a str>,
    pub host: Option<&'a str>,
    pub port: Option<&'a str>,
    pub path: Option<&'a str>,
    pub query: Option<&'a str>,
    pub fragment: Option<&'a str>,
}

impl<'a> Uri<'a> {
    pub fn new(mut src: &'a str) -> Result<Self, Error> {
        let mut uri = Uri {
            scheme: None,
            userinfo: None,
            host: None,
            port: None,
            path: None,
            query: None,
            fragment: None,
        };

        if let Some((rest, frag)) = src.split_once('#') {
            src = rest;
            uri.fragment = Some(frag);
        }
        if let Some((rest, query)) = src.split_once('?') {
            src = rest;
            uri.query = Some(query);
        }

        if src.starts_with(char::is_alphabetic) {
            if let Some((scheme, rest)) = src.split_once(':') {
                if scheme.chars().all(is_scheme) {
                    uri.scheme = Some(scheme);
                    src = rest;
                }
            }
        }

        if let Some(rest) = src.strip_prefix("//") {
            src = rest;
            if let Some((rest, path)) = rest.split_once('/') {
                uri.path = Some(path);
                src = rest;
            }

            if let Some((rest, port)) = src.rsplit_once(':') {
                if port.chars().all(|x| x.is_ascii_digit()) {
                    uri.port = Some(port);
                    src = rest;
                }
            }
            if let Some((userinfo, host)) = src.split_once('@') {
                uri.userinfo = Some(userinfo);
                uri.host = Some(host);
            } else {
                uri.host = Some(src);
            }
        } else {
            uri.path = Some(src);
        }

        Ok(uri)
    }
}
impl<'a> TryFrom<&'a str> for Uri<'a> {
    type Error = Error;
    fn try_from(s: &'a str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}
impl<'a> From<&'a UriOwned> for Uri<'a> {
    fn from(uri: &'a UriOwned) -> Self {
        Self {
            scheme: uri.scheme.as_deref(),
            userinfo: uri.userinfo.as_deref(),
            host: uri.host.as_deref(),
            port: uri.port.as_deref(),
            path: uri.path.as_deref(),
            query: uri.query.as_deref(),
            fragment: uri.fragment.as_deref(),
        }
    }
}

impl std::fmt::Display for Uri<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        if let Some(scheme) = self.scheme {
            write!(f, "{scheme}")?;
            write!(f, ":")?;
        }

        if self.host.is_some() {
            write!(f, "//")?;
            if let Some(userinfo) = self.userinfo {
                write!(f, "{userinfo}")?;
                write!(f, "@")?;
            }
            if let Some(host) = self.host {
                write!(f, "{host}")?;
            }
            if let Some(port) = self.port {
                write!(f, ":")?;
                write!(f, "{port}")?;
            }
            if let Some(path) = self.path {
                write!(f, "/")?;
                write!(f, "{}", path.trim_start_matches("/"))?;
            }
        } else if let Some(path) = self.path {
            write!(f, "{path}")?;
        }
        if let Some(query) = self.query {
            write!(f, "?")?;
            write!(f, "{query}")?;
        }
        if let Some(fragment) = self.fragment {
            write!(f, "#")?;
            write!(f, "{fragment}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriOwned {
    pub scheme: Option<String>,
    pub userinfo: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub path: Option<String>,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

impl From<Uri<'_>> for UriOwned {
    fn from(uri: Uri) -> Self {
        Self {
            scheme: uri.scheme.map(String::from),
            userinfo: uri.userinfo.map(String::from),
            host: uri.host.map(String::from),
            port: uri.port.map(String::from),
            path: uri.path.map(String::from),
            query: uri.query.map(String::from),
            fragment: uri.fragment.map(String::from),
        }
    }
}

impl UriOwned {
    pub fn as_ref(&self) -> Uri {
        self.into()
    }
}

impl std::fmt::Display for UriOwned {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        let uri: Uri = self.into();
        write!(f, "{uri}")
    }
}

fn is_scheme(c: char) -> bool {
    c.is_alphabetic() || c.is_ascii_digit() || "+-.".contains(c)
}

pub fn percent_decode(s: impl AsRef<str>) -> Option<String> {
    let s = s.as_ref();
    let mut out = String::new();
    let mut rem = 0;
    for (i, ch) in s.chars().enumerate() {
        if rem == 0 {
            if ch == '%' {
                rem = 2;
            } else {
                out.push(ch);
            }
            continue;
        }
        rem -= 1;
        if rem == 0 {
            out.push(u8::from_str_radix(&s[i - 1..=i], 16).ok().map(char::from)?);
        }
    }
    Some(out)
}

// TODO: Percent Encode

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent() {
        assert_eq!(
            percent_decode("%21%40%23%24%25%2A%28%29With Some Text in the middle%7E%7B%7D%3A%3C%3E%3F_%2B").unwrap(),
            "!@#$%*()With Some Text in the middle~{}:<>?_+");
    }

    #[test]
    fn uri() {
        let test1 = "ftp://ftp.is.co.za/rfc/rfc1808.txt";
        let test2 = "http://www.ietf.org/rfc/rfc2396.txt";
        let test3 = "ldap://[2001:db8::7]/c=GB?objectClass?one";
        let test4 = "mailto:John.Doe@example.com";
        let test5 = "news:comp.infosystems.www.servers.unix";
        let test6 = "tel:+1-816-555-1212";
        let test7 = "telnet://192.0.2.16:80/";
        let test8 = "urn:oasis:names:specification:docbook:dtd:xml:4.1.2";
        let test9 = "https://datatracker.ietf.org/doc/html/rfc3986#section-1.1.2";
        let test10 = "https://www.youtube.com/watch?v=QyjyWUrHsFc";
        let test11 = "https://john.doe@www.example.com:1234/forum/questions/?query#Frag";
        Uri::new(test1).unwrap();
        Uri::new(test2).unwrap();
        Uri::new(dbg!(test3)).unwrap();
        Uri::new(test4).unwrap();
        Uri::new(test5).unwrap();
        Uri::new(test6).unwrap();
        Uri::new(test7).unwrap();
        let uri2 = Uri::new(test8).unwrap();
        assert_eq!(
            uri2.path,
            Some("oasis:names:specification:docbook:dtd:xml:4.1.2")
        );
        Uri::new(test9).unwrap();
        Uri::new(test10).unwrap();
        let uri = Uri::new(test11).unwrap();
        assert_eq!(uri.scheme, Some("https"));
        assert_eq!(uri.userinfo, Some("john.doe"));
        assert_eq!(uri.host, Some("www.example.com"));
        assert_eq!(uri.port, Some("1234"));
        assert_eq!(uri.path, Some("forum/questions/"));
        assert_eq!(uri.query, Some("query"));
        assert_eq!(uri.fragment, Some("Frag"));
    }

    #[test]
    fn uri_owned() {
        let test1 = "https://www.youtube.com/watch?v=QyjyWUrHsFc";
        let test2 = "http://www.ietf.org/rfc/rfc2396.txt";
        let test3 = "ldap://[2001:db8::7]/c=GB?objectClass?one";
        let test4 = "mailto:John.Doe@example.com";
        let test5 = "news:comp.infosystems.www.servers.unix";
        let test6 = "tel:+1-816-555-1212";
        let test7 = "telnet://192.0.2.16:80/";
        let test8 = "urn:oasis:names:specification:docbook:dtd:xml:4.1.2";

        let uri1 = Uri::new(test1).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri1)).to_string(), test1);
        let uri2 = Uri::new(test2).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri2)).to_string(), test2);
        let uri3 = Uri::new(test3).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri3)).to_string(), test3);
        let uri4 = Uri::new(test4).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri4)).to_string(), test4);
        let uri5 = Uri::new(test5).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri5)).to_string(), test5);
        let uri6 = Uri::new(test6).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri6)).to_string(), test6);
        let uri7 = Uri::new(test7).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri7)).to_string(), test7);
        let uri8 = Uri::new(test8).unwrap();
        assert_eq!(UriOwned::from(dbg!(uri8)).to_string(), test8);
    }
}
