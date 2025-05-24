use anyhow::Result;
use chrono::DateTime;
use log::{
    debug,
    warn,
};
use regex::Regex;

use crate::db::Message;

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
        body: "".to_string(),
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

#[cfg(test)]
mod tests {
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
}
