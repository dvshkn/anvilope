use std::collections::VecDeque;

use anyhow::{
    Error,
    Result
};
use async_imap::imap_proto::{
    BodyStructure,
    ContentEncoding,
    SectionPath,
};
use base64::prelude::{
    BASE64_STANDARD,
    Engine,
};
use chrono::DateTime;
use html_to_markdown_rs::{
    ConversionOptions,
    convert,
};
use log::{
    debug,
    warn,
};
use regex::Regex;
use serde::Serialize;

use crate::db::Message;

#[derive(Debug, Serialize, Eq, PartialEq)]
pub enum TextPartMimeType {
    Plain,
    Html,
}
impl From<String> for TextPartMimeType {
    fn from(s: String) -> Self {
        let s_lower = s.to_lowercase();
        if s_lower == "html" {
            TextPartMimeType::Html
        } else if s_lower == "plain" {
            TextPartMimeType::Plain
        } else {
            warn!("unknown MIME type text/{}", s_lower);
            TextPartMimeType::Plain
        }
    }
}

#[derive(Debug, Serialize, Eq, PartialEq)]
pub enum TextPartEncoding {
    SevenBit,
    EightBit,
    Base64,
    QuotedPrintable,
    Other,
}
impl From<&ContentEncoding<'_>> for TextPartEncoding {
    fn from(ce: &ContentEncoding) -> Self {
        match ce {
            ContentEncoding::SevenBit => TextPartEncoding::SevenBit,
            ContentEncoding::EightBit => TextPartEncoding::EightBit,
            ContentEncoding::Binary => TextPartEncoding::Other,
            ContentEncoding::Base64 => TextPartEncoding::Base64,
            ContentEncoding::QuotedPrintable => TextPartEncoding::QuotedPrintable,
            ContentEncoding::Other(_) => TextPartEncoding::Other,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TextPartDescriptor {
    pub path: Vec<u32>,
    pub mime_subtype: TextPartMimeType,
    pub encoding: TextPartEncoding,
}
impl TextPartDescriptor {
    pub fn to_section_path(&self) -> SectionPath {
        SectionPath::Part(self.path.clone(), None)
    }

    pub fn to_string(&self) -> String {
        self.path.iter().map(|i| i.to_string()).collect::<Vec<String>>().join(".")
    }
}

pub fn parse_header(account_key: &String, bytes: &[u8]) -> Result<Message> {
    let header = std::str::from_utf8(bytes)?;

    let mut from_address = "".to_string();
    let mut from_name = "".to_string();
    let mut subject = "".to_string();
    let mut date: i64 = 0;
    for line in header.lines() {
        if line.starts_with("From: ") {
            let from_str = line.strip_prefix("From: ").unwrap();
            debug!("from value = {}", from_str);
            if let Some((addr, name)) = parse_from_field(from_str) {
                from_address = addr;
                from_name = name;
            } else {
                warn!("could not parse message from field");
            }
        } else if line.starts_with("Subject: ") {
            subject = line.strip_prefix("Subject: ").unwrap().to_string();
            debug!("subject value = {}", subject);
        } else if line.starts_with("Date: ") {
            let date_str = line.strip_prefix("Date: ").unwrap();
            debug!("date value = {}", date_str);
            if let Some(d) = parse_date_field(date_str) {
                date = d;
            } else {
                warn!("could not parse message date field");
            }
        }
    }

    Ok(Message {
        id: 0,
        account_key: account_key.to_owned(),
        class_key: None,
        from_address,
        from_name,
        subject,
        body_plain: "".to_string(),
        body_html: "".to_string(),
        uid: 0,
        uid_validity: 0,
        created_at: date,
    })
}

pub fn parse_from_field(from: &str) -> Option<(String, String)> {
    let re_addr = Regex::new(r"<(?P<email>[^@\s]+@[^@\s]+\.[^@\s]+)>").unwrap();

    if let Some(vals) = re_addr.captures(from) {
        let email = (&vals["email"]).to_string();
        let mut name = from.replace(&format!("<{}>", email), "");
        name = name.trim().to_string();
        if let Some(s) = name.as_str().strip_prefix("\"") {
            name = s.to_string();
        }
        if let Some(s) = name.as_str().strip_suffix("\"") {
            name = s.to_string();
        }
        return Some((email, name));
    }

    // TODO: support ancient format "email@address.com (Full Name)"

    None
}

pub fn parse_date_field(date: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc2822(date) {
        return Some(dt.timestamp_millis());
    }
    None
}

pub fn find_text_parts(root: &BodyStructure) -> Vec<TextPartDescriptor> {
    let mut result: Vec<TextPartDescriptor> = Vec::new();
    let mut body_queue: VecDeque<&BodyStructure> = VecDeque::from([root]);
    let mut path_queue: VecDeque<Vec<u32>> = VecDeque::from([Vec::new()]);

    while body_queue.len() > 0 {
        let body = body_queue.pop_front().unwrap();
        let mut path = path_queue.pop_front().unwrap();
        match body {
            BodyStructure::Basic { common: _, other: _, extension: _ } => {
                // no-op
            },
            BodyStructure::Text { common, other, lines: _, extension: _ } => {
                let is_text = common.ty.ty.to_uppercase() == "TEXT";
                let is_plain = common.ty.subtype.to_uppercase() == "PLAIN";
                let is_html = common.ty.subtype.to_uppercase() == "HTML";
                if is_text && (is_plain || is_html) {
                    if path.len() == 0 { // single-part email case
                        path.push(1);
                    }
                    result.push(TextPartDescriptor {
                        path: path.clone(),
                        mime_subtype: TextPartMimeType::from(common.ty.subtype.to_string()),
                        encoding: TextPartEncoding::from(&other.transfer_encoding),
                    });
                }
            },
            BodyStructure::Message { common: _, other: _, envelope: _, body, lines: _, extension: _ } => {
                // attached or forwarded emails
                let nested_parts = find_text_parts(body);
                for mut np in nested_parts {
                    let mut prepended_path = path.clone();
                    prepended_path.extend_from_slice(&np.path);
                    np.path = prepended_path;
                    result.push(np);
                }
            },
            BodyStructure::Multipart { common: _, bodies, extension: _ } => {
                for (i, inner_body) in bodies.iter().enumerate() {
                    let mut inner_path = path.clone();
                    inner_path.push(u32::try_from(i).unwrap() + 1);
                    body_queue.push_back(inner_body);
                    path_queue.push_back(inner_path);
                }
            }
        }
    }

    return result;
}

pub fn decode_utf8(bytes: &[u8], encoding: &TextPartEncoding) -> Option<String> {
    if *encoding == TextPartEncoding::Base64 {
        let mut clean_bytes = Vec::from(bytes);
        clean_bytes.retain(|&b| b >= 32); // we love \r\n
        let maybe_decoded = BASE64_STANDARD.decode(clean_bytes);
        if let Ok(decoded) = maybe_decoded {
            let maybe_refstr = std::str::from_utf8(decoded.as_slice());
            if let Ok(refstr) = maybe_refstr {
                return Some(refstr.to_string());
            }
        }
    } else if *encoding == TextPartEncoding::QuotedPrintable {
        let maybe_decoded = quoted_printable::decode(bytes, quoted_printable::ParseMode::Robust);
        if let Ok(decoded) = maybe_decoded {
            let maybe_refstr = std::str::from_utf8(decoded.as_slice());
            if let Ok(refstr) = maybe_refstr {
                return Some(refstr.to_string());
            }
        }
    } else if *encoding != TextPartEncoding::Other {
        let maybe_refstr = std::str::from_utf8(bytes);
        if let Ok(refstr) = maybe_refstr {
            return Some(refstr.to_string());
        }
    }
    None
}

/// Converts decoded HTML content to Markdown
pub fn html_to_markdown(html: &str) -> Result<String> {
    let options = ConversionOptions::builder()
        .wrap(false)
        .build();
    let res = html_to_markdown_rs::convert(html, Some(options))?;
    match res.content {
        Some(md) => Ok(md),
        None => Err(Error::msg("markdown conversion returned no content"))
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashSet;
    use async_imap::imap_proto::{BodyContentCommon, BodyContentSinglePart, ContentEncoding, ContentType, Envelope};

    use super::*;

    #[test]
    fn test_parse_header() {
        let key = "glitchmail".to_string();
        let regular_header = "From: \"Alice Rabbit\" <alice@glitch.city>\n".to_string() +
            "Date: 4 May 2025 16:09:49 -0700\n" +
            "Subject: About those \"special\" chips\n";
        let malformed_header = "Sender: alice@glitch.city\nFFFFFFFF\t\t\t\n1/1/25\n".to_string();

        let result = parse_header(&key, regular_header.as_bytes()).unwrap();
        assert_eq!(result.account_key, key);
        assert_eq!(result.from_name, "Alice Rabbit");
        assert_eq!(result.from_address, "alice@glitch.city");
        assert_eq!(result.subject, "About those \"special\" chips");
        assert_eq!(result.created_at, 1746400189000);

        let result = parse_header(&key, malformed_header.as_bytes()).unwrap();
        assert_eq!(result.account_key, key);
        assert_eq!(result.from_name, "");
        assert_eq!(result.from_address, "");
        assert_eq!(result.subject, "");
        assert_eq!(result.created_at, 0);
    }

    #[test]
    fn test_parse_from_field() {
        let name_quotes = "\"Alice Rabbit\" <alice@glitch.city>";
        let name_no_quotes = "Alice Rabbit <alice@glitch.city>";
        let name_no_spaces = "alice_rabbit <alice+snakecase@glitch.city>";
        let bad_email_1 = "Bob Tortoise <robert tortoise@zaibatsu.corp>";
        let bad_email_2 = "Bob Tortoise <robert.tortoise%zaibatsu.corp>";
        let extra_bad = "Robert'); DROP TABLE llm_job; --";

        let result = parse_from_field(name_quotes).unwrap();
        assert_eq!(result.0.as_str(), "alice@glitch.city");
        assert_eq!(result.1.as_str(), "Alice Rabbit");

        let result = parse_from_field(name_no_quotes).unwrap();
        assert_eq!(result.0.as_str(), "alice@glitch.city");
        assert_eq!(result.1.as_str(), "Alice Rabbit");

        let result = parse_from_field(name_no_spaces).unwrap();
        assert_eq!(result.0.as_str(), "alice+snakecase@glitch.city");
        assert_eq!(result.1.as_str(), "alice_rabbit");

        let result = parse_from_field(bad_email_1);
        assert!(result.is_none());

        let result = parse_from_field(bad_email_2);
        assert!(result.is_none());

        let result = parse_from_field(extra_bad);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_date_field() {
        let basic = "05 May 2038 17:29:11 GMT";
        let timezone = "05 May 2038 17:29:11 -0700";
        let day_of_week = "Wed, 5 May 2038 17:29:11 -0700";
        let bad_day_of_week = "Fri, 5 May 2038 17:29:11 -0700";

        let result = parse_date_field(basic).unwrap();
        assert_eq!(result, 2156693351000);

        let result = parse_date_field(timezone).unwrap();
        assert_eq!(result, 2156718551000);

        let result = parse_date_field(day_of_week).unwrap();
        assert_eq!(result, 2156718551000);

        let result = parse_date_field(bad_day_of_week);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_text_parts() {
        // single part structure
        let single_part_text_plain = BodyStructure::Text {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("TEXT"), subtype: Cow::from("PLAIN"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::QuotedPrintable,
                octets: 0,
            },
            lines: 0,
            extension: None,
        };
        let parts = find_text_parts(&single_part_text_plain);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].to_string(), "1");

        // multipart structure example from IMAPv4 RFC
        // https://datatracker.ietf.org/doc/html/rfc3501#page-56
        let text_plain_1 = BodyStructure::Text {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("TEXT"), subtype: Cow::from("PLAIN"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::QuotedPrintable,
                octets: 0,
            },
            lines: 0,
            extension: None,
        };
        let application_octet_stream_2 = BodyStructure::Basic {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("APPLICATION"), subtype: Cow::from("OCTET-STREAM"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::Base64,
                octets: 0,
            },
            extension: None,
        };
        let inner_message_3 = BodyStructure::Message {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("MESSAGE"), subtype: Cow::from("RFC2822"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::EightBit,
                octets: 0,
            },
            envelope: Envelope {
                date: None, subject: None, from: None, sender: None, reply_to: None, to: None, cc: None, bcc: None,
                in_reply_to: None, message_id: None,
            },
            body: Box::new(BodyStructure::Multipart {
                common: BodyContentCommon {
                    ty: ContentType { ty: Cow::from("MULTIPART"), subtype: Cow::from("MIXED"), params: None },
                    disposition: None,
                    language: None,
                    location: None,
                },
                bodies: vec![
                    BodyStructure::Text {
                        common: BodyContentCommon {
                            ty: ContentType { ty: Cow::from("TEXT"), subtype: Cow::from("PLAIN"), params: None },
                            disposition: None,
                            language: None,
                            location: None,
                        },
                        other: BodyContentSinglePart {
                            id: None,
                            md5: None,
                            description: None,
                            transfer_encoding: ContentEncoding::QuotedPrintable,
                            octets: 0,
                        },
                        lines: 0,
                        extension: None,
                    },
                    BodyStructure::Basic {
                        common: BodyContentCommon {
                            ty: ContentType { ty: Cow::from("APPLICATION"), subtype: Cow::from("OCTET-STREAM"), params: None },
                            disposition: None,
                            language: None,
                            location: None,
                        },
                        other: BodyContentSinglePart {
                            id: None,
                            md5: None,
                            description: None,
                            transfer_encoding: ContentEncoding::Base64,
                            octets: 0,
                        },
                        extension: None,
                    },
                ],
                extension: None,
            }),
            lines: 0,
            extension: None,
        };
        let image_gif_4_1 = BodyStructure::Basic {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("IMAGE"), subtype: Cow::from("GIF"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::Base64,
                octets: 0,
            },
            extension: None,
        };
        let inner_message_4_2 = BodyStructure::Message {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("MESSAGE"), subtype: Cow::from("RFC2822"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::EightBit,
                octets: 0,
            },
            envelope: Envelope {
                date: None, subject: None, from: None, sender: None, reply_to: None, to: None, cc: None, bcc: None,
                in_reply_to: None, message_id: None,
            },
            body: Box::new(BodyStructure::Multipart {
                common: BodyContentCommon {
                    ty: ContentType { ty: Cow::from("MULTIPART"), subtype: Cow::from("MIXED"), params: None },
                    disposition: None,
                    language: None,
                    location: None,
                },
                bodies: vec![
                    BodyStructure::Text {
                        common: BodyContentCommon {
                            ty: ContentType { ty: Cow::from("TEXT"), subtype: Cow::from("PLAIN"), params: None },
                            disposition: None,
                            language: None,
                            location: None,
                        },
                        other: BodyContentSinglePart {
                            id: None,
                            md5: None,
                            description: None,
                            transfer_encoding: ContentEncoding::QuotedPrintable,
                            octets: 0,
                        },
                        lines: 0,
                        extension: None,
                    },
                    BodyStructure::Multipart {
                        common: BodyContentCommon {
                            ty: ContentType { ty: Cow::from("MULTIPART"), subtype: Cow::from("ALTERNATIVE"), params: None },
                            disposition: None,
                            language: None,
                            location: None,
                        },
                        bodies: vec![
                            BodyStructure::Text {
                                common: BodyContentCommon {
                                    ty: ContentType { ty: Cow::from("TEXT"), subtype: Cow::from("PLAIN"), params: None },
                                    disposition: None,
                                    language: None,
                                    location: None,
                                },
                                other: BodyContentSinglePart {
                                    id: None,
                                    md5: None,
                                    description: None,
                                    transfer_encoding: ContentEncoding::QuotedPrintable,
                                    octets: 0,
                                },
                                lines: 0,
                                extension: None,
                            },
                            BodyStructure::Text {
                                common: BodyContentCommon {
                                    ty: ContentType { ty: Cow::from("TEXT"), subtype: Cow::from("HTML"), params: None },
                                    disposition: None,
                                    language: None,
                                    location: None,
                                },
                                other: BodyContentSinglePart {
                                    id: None,
                                    md5: None,
                                    description: None,
                                    transfer_encoding: ContentEncoding::QuotedPrintable,
                                    octets: 0,
                                },
                                lines: 0,
                                extension: None,
                            },
                        ],
                        extension: None,
                    },
                ],
                extension: None,
            }),
            lines: 0,
            extension: None,
        };
        let inner_multipart_4 = BodyStructure::Multipart {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("MULTIPART"), subtype: Cow::from("MIXED"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            bodies: vec![
                image_gif_4_1,
                inner_message_4_2,
            ],
            extension: None,
        };
        let multipart_root = BodyStructure::Multipart {
            common: BodyContentCommon {
                ty: ContentType { ty: Cow::from("MULTIPART"), subtype: Cow::from("MIXED"), params: None },
                disposition: None,
                language: None,
                location: None,
            },
            bodies: vec![
                text_plain_1,
                application_octet_stream_2,
                inner_message_3,
                inner_multipart_4,
            ],
            extension: None,
        };

        let parts = find_text_parts(&multipart_root);
        let expected_specifiers = HashSet::from([
            "1",
            "3.1",
            "4.2.1",
            "4.2.2.1",
            "4.2.2.2",
        ]);
        assert_eq!(parts.len(), 5);
        let mut prev_path_len = 0;
        for p in parts {
            let path_str = p.to_string();
            assert!(expected_specifiers.contains(path_str.as_str()));
            assert!(p.path.len() >= prev_path_len);
            prev_path_len = p.path.len();
        }
    }
}
