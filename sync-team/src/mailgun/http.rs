use std::cell::RefCell;
use std::env;
use std::str;

use curl::easy::{Easy, Form};
use failure::{bail, format_err, Error, ResultExt};

pub fn get<T: for<'de> serde::Deserialize<'de>>(url: &str) -> Result<T, Error> {
    execute(url, Method::Get)
}

pub fn post<T: for<'de> serde::Deserialize<'de>>(url: &str, form: Form) -> Result<T, Error> {
    execute(url, Method::Post(form))
}

pub fn put<T: for<'de> serde::Deserialize<'de>>(url: &str, form: Form) -> Result<T, Error> {
    execute(url, Method::Put(form))
}

pub fn delete<T: for<'de> serde::Deserialize<'de>>(url: &str) -> Result<T, Error> {
    execute(url, Method::Delete)
}

pub enum Method {
    Get,
    Delete,
    Post(Form),
    Put(Form),
}

fn execute<T: for<'de> serde::Deserialize<'de>>(url: &str, method: Method) -> Result<T, Error> {
    thread_local!(static HANDLE: RefCell<Easy> = RefCell::new(Easy::new()));
    let password =
        env::var("MAILGUN_API_TOKEN").map_err(|_| format_err!("must set $MAILGUN_API_TOKEN"))?;
    let result = HANDLE.with(|handle| {
        let mut handle = handle.borrow_mut();
        handle.reset();
        let url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            format!("https://api.mailgun.net/v3{}", url)
        };
        handle.url(&url)?;
        match method {
            Method::Get => {
                log::debug!("GET {}", url);
                handle.get(true)?;
            }
            Method::Delete => {
                log::debug!("DELETE {}", url);
                handle.custom_request("DELETE")?;
            }
            Method::Post(form) => {
                log::debug!("POST {}", url);
                handle.httppost(form)?;
            }
            Method::Put(form) => {
                log::debug!("PUT {}", url);
                handle.httppost(form)?;
                handle.custom_request("PUT")?;
            }
        }
        // Add the API key only for Mailgun requests
        if url.starts_with("https://api.mailgun.net") {
            handle.username("api")?;
            handle.password(&password)?;
        }
        handle.useragent("rust-lang/rust membership update")?;
        // handle.verbose(true)?;
        let mut result = Vec::new();
        let mut headers = Vec::new();
        {
            let mut transfer = handle.transfer();
            transfer.write_function(|data| {
                result.extend_from_slice(data);
                Ok(data.len())
            })?;
            transfer.header_function(|header| {
                if let Ok(s) = str::from_utf8(header) {
                    headers.push(s.to_string());
                }
                true
            })?;
            transfer.perform()?;
        }

        let result =
            String::from_utf8(result).map_err(|_| format_err!("response was invalid utf-8"))?;

        log::trace!("headers: {:#?}", headers);
        log::trace!("json: {}", result);
        let code = handle.response_code()?;
        if code != 200 {
            bail!("failed to get a 200 code, got {}\n\n{}", code, result)
        }
        Ok(serde_json::from_str(&result).with_context(|_| "failed to parse json response")?)
    });
    Ok(result.with_context(|_| format!("failed to send request to {}", url))?)
}
