use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::str;

use curl::easy::{Easy, Form};
use failure::{bail, format_err, Error, ResultExt};

#[derive(serde_derive::Deserialize)]
struct Mailmap {
    lists: Vec<List>,
}

#[derive(serde_derive::Deserialize)]
struct List {
    address: String,
    access_level: String,
    members: Vec<String>,
}

mod api {
    #[derive(serde_derive::Deserialize)]
    pub struct ListResponse {
        pub items: Vec<List>,
        pub paging: Paging,
    }

    #[derive(serde_derive::Deserialize)]
    pub struct List {
        pub access_level: String,
        pub address: String,
        pub members_count: u64,
    }

    #[derive(serde_derive::Deserialize)]
    pub struct Paging {
        pub first: String,
        pub last: String,
        pub next: String,
        pub previous: String,
    }

    #[derive(serde_derive::Deserialize)]
    pub struct MembersResponse {
        pub items: Vec<Member>,
        pub paging: Paging,
    }

    #[derive(serde_derive::Deserialize)]
    pub struct Member {
        pub address: String,
    }
}

#[derive(serde_derive::Deserialize)]
struct Empty {}

fn main() {
    env_logger::init();
    if let Err(e) = run() {
        eprintln!("error: {}", e);
        for e in e.iter_causes() {
            eprintln!("  cause: {}", e);
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let mailmap = fs::read_to_string("mailmap.toml")
        .with_context(|_| "failed to read `mailmap.toml`")?;

    let mailmap: Mailmap = toml::from_str(&mailmap)
        .with_context(|_| "failed to deserialize toml mailmap")?;

    let mut lists = Vec::new();
    let mut response = get::<api::ListResponse>("/lists/pages")?;
    while response.items.len() > 0 {
        lists.extend(response.items);
        response = get::<api::ListResponse>(&response.paging.next)?;
    }

    let mut addr2list = HashMap::new();
    for list in mailmap.lists.iter() {
        if addr2list.insert(&list.address, list).is_some() {
            bail!("duplicate address: {}", list.address);
        }
    }

    for prev_list in lists {
        let address = &prev_list.address;
        match addr2list.remove(address) {
            Some(new_list) => {
                sync(&prev_list, &new_list)
                    .with_context(|_| format!("failed to sync {}", address))?
            }
            None => {
                del(&prev_list)
                    .with_context(|_| format!("failed to delete {}", address))?
            }
        }
    }

    for (_, list) in addr2list.iter() {
        create(list)
            .with_context(|_| format!("failed to create {}", list.address))?;
    }

    Ok(())
}

fn create(new: &List) -> Result<(), Error> {
    let mut form = Form::new();
    form.part("address").contents(new.address.as_bytes()).add()?;
    form.part("access_level").contents(new.access_level.as_bytes()).add()?;
    post::<Empty>("/lists", form)?;

    add_members(&new.address, &new.members)?;
    Ok(())
}

fn sync(prev: &api::List, new: &List) -> Result<(), Error> {
    assert_eq!(prev.address, new.address);
    let url = format!("/lists/{}", prev.address);
    if prev.access_level != new.access_level {
        let mut form = Form::new();
        form.part("access_level").contents(new.access_level.as_bytes()).add()?;
        put::<Empty>(&url, form)?;
    }

    let url = format!("{}/members/pages", url);
    let mut prev_members = HashSet::new();
    let mut response = get::<api::MembersResponse>(&url)?;
    while response.items.len() > 0 {
        prev_members.extend(response.items.into_iter().map(|member| member.address));
        response = get::<api::MembersResponse>(&response.paging.next)?;
    }

    let mut to_add = Vec::new();
    for member in new.members.iter() {
        if !prev_members.remove(member) {
            to_add.push(member.clone());
        }
    }

    if to_add.len() > 0 {
        add_members(&new.address, &to_add)?;
    }
    for member in prev_members {
         delete::<Empty>(&format!("/lists/{}/members/{}", new.address, member))?;
    }

    Ok(())
}

fn add_members(address: &str, members: &[String]) -> Result<(), Error> {
    let url = format!("/lists/{}/members.json", address);
    let data = serde_json::to_string(members)?;
    let mut form = Form::new();
    form.part("members").contents(data.as_bytes()).add()?;
    post::<Empty>(&url, form)?;
    Ok(())
}

fn del(prev: &api::List) -> Result<(), Error> {
    delete::<Empty>(&format!("/lists/{}", prev.address))?;
    Ok(())
}

fn get<T: for<'de> serde::Deserialize<'de>>(url: &str) -> Result<T, Error> {
    execute(url, Method::Get)
}

fn post<T: for<'de> serde::Deserialize<'de>>(
    url: &str,
    form: Form,
) -> Result<T, Error> {
    execute(url, Method::Post(form))
}

fn put<T: for<'de> serde::Deserialize<'de>>(
    url: &str,
    form: Form,
) -> Result<T, Error> {
    execute(url, Method::Put(form))
}

fn delete<T: for<'de> serde::Deserialize<'de>>(url: &str) -> Result<T, Error> {
    execute(url, Method::Delete)
}

enum Method {
    Get,
    Delete,
    Post(Form),
    Put(Form),
}

fn execute<T: for<'de> serde::Deserialize<'de>>(
    url: &str,
    method: Method,
) -> Result<T, Error> {
    thread_local!(static HANDLE: RefCell<Easy> = RefCell::new(Easy::new()));
    let password = env::var("MAILGUN_API_TOKEN")
        .map_err(|_| format_err!("must set $MAILGUN_API_TOKEN"))?;
    let result = HANDLE.with(|handle| {
        let mut handle = handle.borrow_mut();
        handle.reset();
        let url = if url.starts_with("https://") {
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
        handle.username("api")?;
        handle.password(&password)?;
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

        let result = String::from_utf8(result)
            .map_err(|_| format_err!("response was invalid utf-8"))?;

        log::trace!("headers: {:#?}", headers);
        log::trace!("json: {}", result);
        let code = handle.response_code()?;
        if code != 200 {
            bail!("failed to get a 200 code, got {}\n\n{}", code, result)
        }
        Ok(serde_json::from_str(&result)
            .with_context(|_| "failed to parse json response")?)
    });
    Ok(result.with_context(|_| format!("failed to send request to {}", url))?)
}
