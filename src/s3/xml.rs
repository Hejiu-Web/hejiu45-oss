//! XML rendering helpers shared by the S3 handlers.
//!
//! Hand-written string building rather than a derive-based serializer: the
//! S3 documents that matter here are small and fixed, and hand-writing them
//! keeps the wire format (namespaces, element order, escaping) explicit and
//! testable.

use crate::error::xml_escape;

/// Wrap an S3 sub-resource body in the standard envelope.
pub fn envelope(root: &str, inner: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<{root} xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{inner}</{root}>"
    )
}

/// Minimal XML element reader — deliberately small and lenient.
///
/// Accepts attributes, self-closing tags and the namespace prefixes S3 SDKs
/// emit, and ignores comments and the declaration. Enough for
/// `DeleteObjects`, `CompleteMultipartUpload` and the bucket-create config.
#[derive(Debug, Default)]
pub struct XmlReader {
    doc: Vec<char>,
    pos: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlNode {
    pub name: String,
    pub text: String,
    pub children: Vec<XmlNode>,
}

impl XmlReader {
    pub fn new(src: &str) -> Self {
        // Drop the declaration and comments before scanning.
        let mut cleaned = String::with_capacity(src.len());
        let mut rest = src;
        loop {
            if let Some(start) = rest.find("<!--") {
                cleaned.push_str(&rest[..start]);
                match rest[start..].find("-->") {
                    Some(end) => rest = &rest[start + end + 3..],
                    None => break,
                }
            } else {
                cleaned.push_str(rest);
                break;
            }
        }
        Self {
            doc: cleaned.chars().collect(),
            pos: 0,
        }
    }

    /// Parse the whole document and return the first element.
    pub fn parse(mut self) -> Result<XmlNode, String> {
        self.skip_ws_and_decl();
        if self.peek() != Some('<') {
            return Err("expected element".into());
        }
        self.parse_element()
            .map_err(|e| format!("XML parse error: {e}"))
    }

    fn peek(&self) -> Option<char> {
        self.doc.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.doc.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws_and_decl(&mut self) {
        loop {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.bump();
            }
            if self.starts_with("<?") {
                while self.pos < self.doc.len() {
                    if self.starts_with("?>") {
                        self.pos += 2;
                        break;
                    }
                    self.bump();
                }
                continue;
            }
            if self.starts_with("<!") {
                while self.pos < self.doc.len() && self.peek() != Some('>') {
                    self.bump();
                }
                self.bump();
                continue;
            }
            break;
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.doc.get(self.pos + i) == Some(&c))
    }

    fn read_name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '>' || c == '/' {
                break;
            }
            name.push(c);
            self.bump();
        }
        name
    }

    fn skip_attributes(&mut self) {
        let mut quote: Option<char> = None;
        while let Some(c) = self.peek() {
            match quote {
                Some(q) if c == q => {
                    quote = None;
                    self.bump();
                }
                Some(_) => {
                    self.bump();
                }
                None => match c {
                    '"' | '\'' => {
                        quote = Some(c);
                        self.bump();
                    }
                    '>' => break,
                    _ => {
                        self.bump();
                    }
                },
            }
        }
    }

    fn parse_element(&mut self) -> Result<XmlNode, String> {
        if self.bump() != Some('<') {
            return Err("expected '<'".into());
        }
        let raw_name = self.read_name();
        if raw_name.is_empty() {
            return Err("empty element name".into());
        }
        // Strip a namespace prefix (`s3:Key` -> `Key`).
        let name = raw_name
            .rsplit_once(':')
            .map(|(_, n)| n.to_string())
            .unwrap_or(raw_name);
        // A self-closing tag has no attributes to skip past; detect it before
        // `skip_attributes` consumes the `/` that signals it.
        if self.starts_with("/>") {
            self.pos += 2;
            return Ok(XmlNode {
                name,
                text: String::new(),
                children: Vec::new(),
            });
        }
        self.skip_attributes();
        if self.starts_with("/>") {
            self.pos += 2;
            return Ok(XmlNode {
                name,
                text: String::new(),
                children: Vec::new(),
            });
        }
        if self.bump() != Some('>') {
            return Err(format!("malformed start tag <{name}>"));
        }

        let mut text = String::new();
        let mut children = Vec::new();
        loop {
            match self.peek() {
                None => return Err(format!("unclosed element <{name}>")),
                Some('<') if self.starts_with("</") => {
                    self.pos += 2;
                    let closing = self.read_name();
                    while matches!(self.peek(), Some(c) if c != '>') {
                        self.bump();
                    }
                    self.bump(); // '>'
                    let closing = closing
                        .rsplit_once(':')
                        .map(|(_, n)| n.to_string())
                        .unwrap_or(closing);
                    if closing != name {
                        return Err(format!("mismatched closing tag </{closing}> for <{name}>"));
                    }
                    return Ok(XmlNode {
                        name,
                        text: unescape(text.trim()),
                        children,
                    });
                }
                Some('<') if self.starts_with("<![CDATA[") => {
                    self.pos += 9;
                    let start = self.pos;
                    while self.pos < self.doc.len() && !self.starts_with("]]>") {
                        self.pos += 1;
                    }
                    let chunk: String = self.doc[start..self.pos].iter().collect();
                    text.push_str(&chunk);
                    self.pos += 3;
                }
                Some('<') => {
                    self.skip_ws_and_decl();
                    children.push(self.parse_element()?);
                }
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
    }

    /// Find a direct child by local name.
    pub fn child<'a>(node: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
        node.children.iter().find(|c| c.name == name)
    }

    /// Text of a direct child.
    pub fn child_text<'a>(node: &'a XmlNode, name: &str) -> Option<&'a str> {
        Self::child(node, name).map(|c| c.text.as_str())
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|e| *e <= 12);
        let Some(end) = end else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                if let Ok(n) = u32::from_str_radix(&ent[2..], 16) {
                    if let Some(c) = char::from_u32(n) {
                        out.push(c);
                    }
                }
            }
            _ if ent.starts_with('#') => {
                if let Ok(n) = ent[1..].parse::<u32>() {
                    if let Some(c) = char::from_u32(n) {
                        out.push(c);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Convenience wrapper used by the handlers.
pub fn escape(s: &str) -> String {
    xml_escape(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delete_objects_document() {
        let doc = r#"<?xml version="1.0"?>
<Delete xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Object><Key>a.txt</Key><VersionId>v1</VersionId></Object>
  <Object><Key>dir/b &amp; c.txt</Key></Object>
  <Quiet>true</Quiet>
</Delete>"#;
        let node = XmlReader::new(doc).parse().unwrap();
        assert_eq!(node.name, "Delete");
        let objs: Vec<&XmlNode> = node
            .children
            .iter()
            .filter(|c| c.name == "Object")
            .collect();
        assert_eq!(objs.len(), 2);
        assert_eq!(XmlReader::child_text(objs[0], "Key"), Some("a.txt"));
        assert_eq!(XmlReader::child_text(objs[1], "Key"), Some("dir/b & c.txt"));
        assert_eq!(XmlReader::child_text(&node, "Quiet"), Some("true"));
    }

    #[test]
    fn parses_complete_multipart_body() {
        let doc = r#"<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>"abc"</ETag></Part><Part><PartNumber>2</PartNumber><ETag>def</ETag></Part></CompleteMultipartUpload>"#;
        let node = XmlReader::new(doc).parse().unwrap();
        let parts: Vec<(String, String)> = node
            .children
            .iter()
            .filter(|c| c.name == "Part")
            .map(|p| {
                (
                    XmlReader::child_text(p, "PartNumber").unwrap().to_string(),
                    XmlReader::child_text(p, "ETag").unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            parts,
            vec![("1".into(), "\"abc\"".into()), ("2".into(), "def".into())]
        );
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(XmlReader::new("<Delete><Object>").parse().is_err());
        assert!(XmlReader::new("<a></b>").parse().is_err());
        assert!(XmlReader::new("not xml").parse().is_err());
    }

    #[test]
    fn handles_prefixes_cdata_and_self_closing() {
        let doc = r#"<s3:Delete><s3:Object><s3:Key><![CDATA[x<y]]></s3:Key></s3:Object><s3:Quiet/></s3:Delete>"#;
        let node = XmlReader::new(doc)
            .parse()
            .unwrap_or_else(|e| panic!("{e} | src={doc}"));
        assert_eq!(node.name, "Delete");
        let obj = XmlReader::child(&node, "Object")
            .unwrap_or_else(|| panic!("no Object child; children={:?}", node.children));
        assert_eq!(XmlReader::child_text(obj, "Key"), Some("x<y"));
        assert!(XmlReader::child(&node, "Quiet").is_some());
    }

    #[test]
    fn envelope_carries_namespace() {
        let s = envelope("ListBucketResult", "<Name>b</Name>");
        assert!(s.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""));
        assert!(s.contains("<Name>b</Name>"));
    }
}
