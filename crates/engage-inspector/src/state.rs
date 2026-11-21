use std::collections::BTreeMap;

use leptos::prelude::*;
use serde_json::{Value, json};

use crate::{api, content};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Conversations,
    Tasks,
    Activity,
    Schedules,
    Settings,
}

#[derive(Clone, Copy)]
pub struct Console {
    pub section: RwSignal<Section>,
    pub channel: RwSignal<String>,
    pub channels: RwSignal<Vec<Value>>,
    pub items: RwSignal<Vec<Value>>,
    pub selected: RwSignal<String>,
    pub thread: RwSignal<Value>,
    pub me: RwSignal<Value>,
    pub loading: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub error: RwSignal<String>,
    pub authenticated: RwSignal<bool>,
    pub checking_auth: RwSignal<bool>,
    pub search: RwSignal<String>,
    pub by: RwSignal<String>,
    pub source: RwSignal<String>,
    pub labels: RwSignal<String>,
    pub task_filter: RwSignal<String>,
    pub cursor: RwSignal<String>,
    pub older: RwSignal<String>,
    pub pages: RwSignal<Vec<String>>,
    pub composing: RwSignal<bool>,
    pub claims: RwSignal<BTreeMap<String, Value>>,
    pub refresh_id: RwSignal<u64>,
}

impl Console {
    pub fn new() -> Self {
        let params = web_sys::UrlSearchParams::new_with_str(
            &window().location().search().unwrap_or_default(),
        )
        .ok();
        let channel = params
            .as_ref()
            .and_then(|params| params.get("channel"))
            .unwrap_or_default();
        let selected = params
            .as_ref()
            .and_then(|params| params.get("thread"))
            .unwrap_or_default();
        let parameter = |key| {
            params
                .as_ref()
                .and_then(|params| params.get(key))
                .unwrap_or_default()
        };
        let section = match parameter("view").as_str() {
            "tasks" => Section::Tasks,
            "activity" => Section::Activity,
            "schedules" => Section::Schedules,
            "settings" => Section::Settings,
            _ => Section::Conversations,
        };
        let task_filter = parameter("state");
        Self {
            section: RwSignal::new(section),
            channel: RwSignal::new(channel),
            channels: RwSignal::new(Vec::new()),
            items: RwSignal::new(Vec::new()),
            selected: RwSignal::new(selected),
            thread: RwSignal::new(Value::Null),
            me: RwSignal::new(Value::Null),
            loading: RwSignal::new(true),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            authenticated: RwSignal::new(false),
            checking_auth: RwSignal::new(true),
            search: RwSignal::new(parameter("text")),
            by: RwSignal::new(parameter("by")),
            source: RwSignal::new(parameter("from")),
            labels: RwSignal::new(parameter("label")),
            task_filter: RwSignal::new(if task_filter.is_empty() {
                "open".into()
            } else {
                task_filter
            }),
            cursor: RwSignal::new(String::new()),
            older: RwSignal::new(String::new()),
            pages: RwSignal::new(Vec::new()),
            composing: RwSignal::new(false),
            claims: RwSignal::new(BTreeMap::new()),
            refresh_id: RwSignal::new(0),
        }
    }

    pub fn report(self, error: api::ApiError) {
        if error.status == 401 {
            self.authenticated.set(false);
            self.claims.set(BTreeMap::new());
            api::set_profile(String::new());
        }
        self.error.set(error.message);
    }

    pub fn boot(self) {
        leptos::task::spawn_local(async move {
            match api::get("/me").await {
                Ok(me) => {
                    self.me.set(me);
                    self.authenticated.set(true);
                    self.refresh_channels().await;
                    self.refresh().await;
                    self.refresh_thread().await;
                }
                Err(error) => self.report(error),
            }
            self.checking_auth.set(false);
            self.loading.set(false);
        });
    }

    pub fn reload(self) {
        self.update_url();
        leptos::task::spawn_local(async move {
            self.refresh_channels().await;
            self.refresh().await;
            self.refresh_thread().await;
        });
    }

    pub async fn refresh_channels(self) {
        match api::get("/channels?limit=500").await {
            Ok(value) => {
                let channels = content::list(&value, "channels");
                if self.channel.get_untracked().is_empty()
                    && let Some(first) = channels.first()
                {
                    self.channel.set(content::field(first, "path").to_owned());
                }
                self.channels.set(channels);
            }
            Err(error) => self.report(error),
        }
    }

    pub async fn refresh(self) {
        let section = self.section.get_untracked();
        let channel = self.channel.get_untracked();
        let search = self.search.get_untracked();
        let state = self.task_filter.get_untracked();
        let (path, key) = match section {
            Section::Conversations => ("/threads", "threads"),
            Section::Tasks => ("/tasks", "tasks"),
            Section::Activity => ("/events", "events"),
            Section::Schedules => ("/schedules", "schedules"),
            Section::Settings => return,
        };
        let mut params = vec![("channel", channel.clone()), ("limit", "100".into())];
        if section == Section::Activity {
            params = vec![
                ("channel", channel.clone()),
                ("last", "100".into()),
                ("all", "true".into()),
            ];
        } else if section == Section::Tasks {
            params.push(("state", state));
        } else if section == Section::Conversations {
            params.push(("text", search));
        } else if section == Section::Schedules {
            params = vec![("channel", channel.clone()), ("all", "true".into())];
        }
        if matches!(
            section,
            Section::Conversations | Section::Tasks | Section::Activity
        ) {
            params.push(("by", self.by.get_untracked()));
            if section != Section::Tasks {
                params.push(("from", self.source.get_untracked()));
            }
            for label in self
                .labels
                .get_untracked()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                params.push(("label", label.to_owned()));
            }
        }
        let response = api::get(&api::query(path, &params)).await;
        let response = if section == Section::Conversations {
            match response {
                Ok(mut value) => {
                    let mut items = content::list(&value, key);
                    for cursor in self.pages.get_untracked() {
                        let mut page_params = params.clone();
                        page_params.push(("before", cursor));
                        match api::get(&api::query(path, &page_params)).await {
                            Ok(page) => {
                                items.extend(content::list(&page, key));
                                value["older"] = page["older"].clone();
                            }
                            Err(error) => {
                                self.report(error);
                                return;
                            }
                        }
                    }
                    let mut seen = std::collections::BTreeSet::new();
                    items.retain(|item| {
                        seen.insert(content::field(content::message(item), "id").to_owned())
                    });
                    value[key] = items.into();
                    Ok(value)
                }
                Err(error) => Err(error),
            }
        } else {
            response
        };
        match response {
            Ok(value)
                if self.section.get_untracked() == section
                    && self.channel.get_untracked() == channel =>
            {
                self.items.set(content::list(&value, key));
                self.older.set(content::field(&value, "older").to_owned());
                self.cursor.set(content::field(&value, "cursor").to_owned());
                self.refresh_id.update(|id| *id += 1);
            }
            Ok(_) => (),
            Err(error) => self.report(error),
        }
    }

    pub async fn refresh_thread(self) {
        let selected = self.selected.get_untracked();
        if selected.is_empty() {
            return;
        }
        match api::get(&format!(
            "/messages/{}?limit=500",
            urlencoding::encode(&selected)
        ))
        .await
        {
            Ok(value) if self.selected.get_untracked() == selected => self.thread.set(value),
            Ok(_) => (),
            Err(error) => self.report(error),
        }
    }

    pub fn open_thread(self, id: String) {
        self.selected.set(id);
        self.thread.set(Value::Null);
        self.composing.set(false);
        self.update_url();
        leptos::task::spawn_local(async move {
            self.refresh_thread().await;
        });
    }

    pub fn select_channel(self, channel: String) {
        self.channel.set(channel);
        self.selected.set(String::new());
        self.thread.set(Value::Null);
        self.search.set(String::new());
        self.pages.set(Vec::new());
        self.by.set(String::new());
        self.source.set(String::new());
        self.labels.set(String::new());
        self.composing.set(false);
        self.update_url();
        self.reload();
    }

    pub fn select_section(self, section: Section) {
        self.section.set(section);
        self.selected.set(String::new());
        self.composing.set(false);
        self.pages.set(Vec::new());
        self.reload();
    }

    fn update_url(self) {
        let section = self.section.get_untracked();
        let params = api::query(
            "",
            &[
                ("channel", self.channel.get_untracked()),
                ("thread", self.selected.get_untracked()),
                (
                    "view",
                    match section {
                        Section::Conversations => "",
                        Section::Tasks => "tasks",
                        Section::Activity => "activity",
                        Section::Schedules => "schedules",
                        Section::Settings => "settings",
                    }
                    .into(),
                ),
                (
                    "state",
                    if section == Section::Tasks {
                        self.task_filter.get_untracked()
                    } else {
                        String::new()
                    },
                ),
                ("text", self.search.get_untracked()),
                ("by", self.by.get_untracked()),
                ("from", self.source.get_untracked()),
                ("label", self.labels.get_untracked()),
            ],
        );
        if let Ok(history) = window().history() {
            let _ = history.replace_state_with_url(
                &wasm_bindgen::JsValue::NULL,
                "",
                Some(&format!(
                    "{}{}",
                    window().location().pathname().unwrap_or_default(),
                    params
                )),
            );
        }
    }

    pub async fn mutate(self, method: &str, path: &str, body: Value) -> Result<Value, ()> {
        if self.busy.get_untracked() {
            return Err(());
        }
        self.busy.set(true);
        self.error.set(String::new());
        let result = api::write(method, path, &body).await;
        self.busy.set(false);
        match result {
            Ok(value) => {
                self.reload();
                Ok(value)
            }
            Err(error) => {
                self.report(error);
                Err(())
            }
        }
    }

    pub fn task_action(self, id: String, action: &str, mut body: Value) {
        let action = action.to_owned();
        if let Some(claim) = self.claims.get_untracked().get(&id)
            && matches!(
                action.as_str(),
                "renew" | "release" | "complete" | "fail" | "block"
            )
        {
            body["epoch"] = claim["epoch"].clone();
            body["token"] = claim["token"].clone();
        }
        leptos::task::spawn_local(async move {
            let path = format!("/tasks/{}/{}", urlencoding::encode(&id), action);
            if let Ok(value) = self.mutate("POST", &path, body).await {
                if action == "claim" {
                    if let Some(claim) = value.get("claim") {
                        self.claims.update(|claims| {
                            claims.insert(id.clone(), claim.clone());
                        });
                    }
                } else if matches!(
                    action.as_str(),
                    "release" | "complete" | "fail" | "block" | "cancel"
                ) {
                    self.claims.update(|claims| {
                        claims.remove(&id);
                    });
                }
            }
        });
    }

    pub fn start_live(self) {
        leptos::task::spawn_local(async move {
            let mut cursor = String::new();
            loop {
                if !self.authenticated.get_untracked() || document().hidden() {
                    gloo_timers::future::TimeoutFuture::new(2_000).await;
                    continue;
                }
                if cursor.is_empty() {
                    cursor = self.cursor.get_untracked();
                }
                let path = api::query(
                    "/events",
                    &[
                        ("after", cursor.clone()),
                        ("wait", "25".into()),
                        ("all", "true".into()),
                    ],
                );
                match api::get(&path).await {
                    Ok(value) => {
                        let first = cursor.is_empty();
                        cursor = content::field(&value, "cursor").to_owned();
                        if first || !content::list(&value, "events").is_empty() {
                            self.refresh_channels().await;
                            self.refresh().await;
                            self.refresh_thread().await;
                        }
                    }
                    Err(error) => {
                        self.report(error);
                        gloo_timers::future::TimeoutFuture::new(2_000).await;
                    }
                }
            }
        });
    }

    pub fn new_channel(self, path: String) {
        if path.trim().is_empty() {
            return;
        }
        leptos::task::spawn_local(async move {
            if self
                .mutate("POST", "/channels", json!({"path": path}))
                .await
                .is_ok()
            {
                self.select_channel(path);
            }
        });
    }
}
