use failure::Error;
use log::info;
use reqwest::{
    header::{self, HeaderValue},
    Client, Method, RequestBuilder,
};

pub(super) struct Mailgun {
    token: String,
    client: Client,
    dry_run: bool,
}

impl Mailgun {
    pub(super) fn new(token: &str, dry_run: bool) -> Self {
        Self {
            token: token.into(),
            client: Client::new(),
            dry_run,
        }
    }

    pub(super) fn get_routes(&self, skip: Option<usize>) -> Result<RoutesResponse, Error> {
        let url = if let Some(skip) = skip {
            format!("routes?skip={}", skip)
        } else {
            "routes".into()
        };
        Ok(self
            .request(Method::GET, &url)
            .send()?
            .error_for_status()?
            .json()?)
    }

    pub(super) fn create_route(
        &self,
        priority: i32,
        description: &str,
        expression: &str,
        actions: &[String],
    ) -> Result<(), Error> {
        if self.dry_run {
            return Ok(());
        }

        let priority_str = priority.to_string();
        let mut form = vec![
            ("priority", priority_str.as_str()),
            ("description", description),
            ("expression", expression),
        ];
        for action in actions {
            form.push(("action", action.as_str()));
        }

        self.request(Method::POST, "routes")
            .form(&form)
            .send()?
            .error_for_status()?;

        Ok(())
    }

    pub(super) fn update_route(
        &self,
        id: &str,
        priority: i32,
        actions: &[String],
    ) -> Result<(), Error> {
        if self.dry_run {
            return Ok(());
        }

        let priority_str = priority.to_string();
        let mut form = vec![("priority", priority_str.as_str())];
        for action in actions {
            form.push(("action", action.as_str()));
        }

        self.request(Method::PUT, &format!("routes/{}", id))
            .form(&form)
            .send()?
            .error_for_status()?;

        Ok(())
    }

    pub(super) fn delete_route(&self, id: &str) -> Result<(), Error> {
        info!("deleting route with ID {}", id);
        if self.dry_run {
            return Ok(());
        }

        self.request(Method::DELETE, &format!("routes/{}", id))
            .send()?
            .error_for_status()?;
        Ok(())
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        let url = if url.starts_with("https://") {
            url.into()
        } else {
            format!("https://api.mailgun.net/v3/{}", url)
        };

        self.client
            .request(method, &url)
            .basic_auth("api", Some(&self.token))
            .header(
                header::USER_AGENT,
                HeaderValue::from_static(crate::USER_AGENT),
            )
    }
}

#[derive(serde::Deserialize)]
pub(super) struct RoutesResponse {
    pub(super) items: Vec<Route>,
    pub(super) total_count: usize,
}

#[derive(serde::Deserialize)]
pub(super) struct Route {
    pub(super) actions: Vec<String>,
    pub(super) expression: String,
    pub(super) id: String,
    pub(super) priority: i32,
    pub(super) description: serde_json::Value,
}
