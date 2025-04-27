use anyhow::Result;
use chrono::DateTime;
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
            let from = line.strip_prefix("From: ").unwrap();
            (from_address, from_name) = parse_from_field(from);
            println!("from value = {}", from);
        } else if line.starts_with("Subject: ") {
            subject = line.strip_prefix("Subject: ").unwrap().to_string();
            println!("subject value = {}", subject);
        } else if line.starts_with("Date: ") {
            let date_str = line.strip_prefix("Date: ").unwrap();
            date = parse_date_field(date_str).unwrap();
            println!("date value = {}", date);
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

pub fn parse_from_field(from: &str) -> (String, String) {
    let mut email = "".to_string();
    let mut name = "".to_string();

    let re_addr = Regex::new(r"<(?P<email>[^@\s]+@[^@\s]+\.[^@\s]+)>").unwrap();

    if let Some(vals) = re_addr.captures(from) {
        email = (&vals["email"]).to_string();
        name = from.replace(&format!("<{}>", email), "");
        name = name.trim().to_string();
        if let Some(s) = name.as_str().strip_prefix("\"") {
            name = s.to_string();
        }
        if let Some(s) = name.as_str().strip_suffix("\"") {
            name = s.to_string();
        }
    }

    // TODO: support ancient format "email@address.com (Full Name)"

    (email, name)
}

pub fn parse_date_field(date: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc2822(date) {
        return Some(dt.timestamp_millis());
    }
    None
}
