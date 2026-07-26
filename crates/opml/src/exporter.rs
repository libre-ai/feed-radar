//! OPML exporter

use feedmind_domain::opml::{OpmlDocument, OpmlOutline};

/// OPML exporter
pub struct OpmlExporter;

impl OpmlExporter {
    /// Export document to OPML 2.0 string
    pub fn export(doc: &OpmlDocument) -> String {
        let mut output = String::new();

        output.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        output.push('\n');
        output.push_str(r#"<opml version="2.0">"#);
        output.push('\n');

        // Head
        output.push_str("  <head>\n");
        if let Some(ref title) = doc.title {
            output.push_str(&format!("    <title>{}</title>\n", Self::escape_xml(title)));
        }
        if let Some(ref date) = doc.date_created {
            output.push_str(&format!(
                "    <dateCreated>{}</dateCreated>\n",
                Self::escape_xml(date)
            ));
        }
        if let Some(ref email) = doc.owner_email {
            output.push_str(&format!(
                "    <ownerEmail>{}</ownerEmail>\n",
                Self::escape_xml(email)
            ));
        }
        output.push_str("  </head>\n");

        // Body
        output.push_str("  <body>\n");
        for outline in &doc.outlines {
            Self::write_outline(&mut output, outline, 2);
        }
        output.push_str("  </body>\n");

        output.push_str("</opml>\n");

        output
    }

    /// Write a single outline (recursive)
    fn write_outline(output: &mut String, outline: &OpmlOutline, indent: usize) {
        let spaces = "  ".repeat(indent);

        output.push_str(&spaces);
        output.push_str("<outline");

        // Required: text
        output.push_str(&format!(r#" text="{}""#, Self::escape_xml(&outline.text)));

        // Optional: title
        if let Some(ref title) = outline.title {
            if title != &outline.text {
                output.push_str(&format!(r#" title="{}""#, Self::escape_xml(title)));
            }
        }

        // Optional: type
        if let Some(ref t) = outline.outline_type {
            output.push_str(&format!(r#" type="{}""#, Self::escape_xml(t)));
        }

        // Optional: xmlUrl
        if let Some(ref url) = outline.xml_url {
            output.push_str(&format!(r#" xmlUrl="{}""#, Self::escape_xml(url)));
        }

        // Optional: htmlUrl
        if let Some(ref url) = outline.html_url {
            output.push_str(&format!(r#" htmlUrl="{}""#, Self::escape_xml(url)));
        }

        // Optional: description
        if let Some(ref desc) = outline.description {
            output.push_str(&format!(r#" description="{}""#, Self::escape_xml(desc)));
        }

        if outline.children.is_empty() {
            output.push_str("/>\n");
        } else {
            output.push_str(">\n");
            for child in &outline.children {
                Self::write_outline(output, child, indent + 1);
            }
            output.push_str(&spaces);
            output.push_str("</outline>\n");
        }
    }

    /// Escape special XML characters
    fn escape_xml(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_structure() {
        let mut doc = OpmlDocument::new(Some("Test Export".to_string()));

        let mut folder = OpmlOutline::folder("Tech".to_string());
        folder.children.push(OpmlOutline::feed(
            "Hacker News".to_string(),
            "https://news.ycombinator.com/rss".to_string(),
            Some("https://news.ycombinator.com".to_string()),
        ));
        doc.outlines.push(folder);
        doc.outlines.push(OpmlOutline::feed(
            "Example".to_string(),
            "https://example.com/feed".to_string(),
            None,
        ));

        let exported = OpmlExporter::export(&doc);

        // Verify structure without parsing
        assert!(exported.contains("<opml version=\"2.0\">"));
        assert!(exported.contains("<title>Test Export</title>"));
        assert!(exported.contains("text=\"Tech\""));
        assert!(exported.contains("text=\"Hacker News\""));
        assert!(exported.contains("xmlUrl=\"https://news.ycombinator.com/rss\""));
        assert!(exported.contains("text=\"Example\""));
    }

    #[test]
    fn test_escape_xml() {
        let doc = OpmlDocument {
            title: Some("Test & <Special> \"Characters\"".to_string()),
            date_created: None,
            owner_email: None,
            outlines: vec![],
        };

        let exported = OpmlExporter::export(&doc);
        assert!(exported.contains("Test &amp; &lt;Special&gt; &quot;Characters&quot;"));
    }
}
