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
    pub thread_from_start: RwSignal<bool>,
    pub reply_pages: RwSignal<Vec<String>>,
    pub reply_cursor: RwSignal<String>,
    pub loading_replies: RwSignal<bool>,
    thread_refresh_id: RwSignal<u64>,
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
    pub loading_more: RwSignal<bool>,
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
            thread_from_start: RwSignal::new(false),
            reply_pages: RwSignal::new(Vec::new()),
            reply_cursor: RwSignal::new(String::new()),
            loading_replies: RwSignal::new(false),
            thread_refresh_id: RwSignal::new(0),
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
            loading_more: RwSignal::new(false),
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
        // Invalidate in-flight reads before waiting for the channel refresh.
        self.refresh_id.update(|id| *id = id.wrapping_add(1));
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
        self.refresh_id.update(|id| *id = id.wrapping_add(1));
        let refresh_id = self.refresh_id.get_untracked();
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
        let response = if matches!(section, Section::Conversations | Section::Tasks) {
            match response {
                Ok(mut value) => {
                    let mut items = content::list(&value, key);
                    let mut continuation = content::page_cursor(&value, key, 100);
                    for cursor in self.pages.get_untracked() {
                        let mut page_params = params.clone();
                        page_params.push((
                            if section == Section::Tasks {
                                "after"
                            } else {
                                "before"
                            },
                            cursor,
                        ));
                        match api::get(&api::query(path, &page_params)).await {
                            Ok(page) => {
                                continuation = content::page_cursor(&page, key, 100);
                                items = content::merge_items(items, content::list(&page, key));
                            }
                            Err(error) => {
                                self.report(error);
                                return;
                            }
                        }
                    }
                    value["older"] = continuation.into();
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
                    && self.channel.get_untracked() == channel
                    && self.refresh_id.get_untracked() == refresh_id =>
            {
                self.items.set(content::list(&value, key));
                self.older.set(content::field(&value, "older").to_owned());
                self.cursor.set(content::field(&value, "cursor").to_owned());
            }
            Ok(_) => (),
            Err(error) => self.report(error),
        }
    }

    pub fn load_more(self) {
        let cursor = self.older.get_untracked();
        if cursor.is_empty() || self.loading_more.get_untracked() {
            return;
        }
        self.pages.update(|pages| {
            if !pages.contains(&cursor) {
                pages.push(cursor);
            }
        });
        self.loading_more.set(true);
        leptos::task::spawn_local(async move {
            self.refresh().await;
            self.loading_more.set(false);
        });
    }

    pub async fn refresh_thread(self) {
        let selected = self.selected.get_untracked();
        if selected.is_empty() {
            return;
        }
        self.thread_refresh_id.update(|id| *id = id.wrapping_add(1));
        let refresh_id = self.thread_refresh_id.get_untracked();
        let from_start = self.thread_from_start.get_untracked();
        let path = format!("/messages/{}", urlencoding::encode(&selected));
        let root = content::field(content::message(&self.thread.get_untracked()), "id").to_owned();
        let mut params = vec![("limit", "500".into())];
        if from_start {
            params.push(("after", root));
        }
        let response = async {
            let mut value = api::get(&api::query(&path, &params)).await?;
            let mut cursor = content::page_cursor(&value, "replies", 500);
            let mut replies = content::list(&value, "replies");
            if from_start {
                for after in self.reply_pages.get_untracked() {
                    let page = api::get(&api::query(
                        &path,
                        &[("limit", "500".into()), ("after", after)],
                    ))
                    .await?;
                    cursor = content::page_cursor(&page, "replies", 500);
                    replies = content::merge_items(replies, content::list(&page, "replies"));
                }
            }
            value["replies"] = replies.into();
            Ok::<_, api::ApiError>((value, cursor))
        }
        .await;
        match response {
            Ok((value, cursor))
                if self.selected.get_untracked() == selected
                    && self.thread_refresh_id.get_untracked() == refresh_id
                    && self.thread_from_start.get_untracked() == from_start =>
            {
                self.thread.set(value);
                self.reply_cursor.set(cursor);
            }
            Ok(_) => (),
            Err(error) => self.report(error),
        }
    }

    pub fn read_from_start(self) {
        self.thread_from_start.set(true);
        self.reply_pages.set(Vec::new());
        self.reload_replies();
    }

    pub fn latest_replies(self) {
        self.thread_from_start.set(false);
        self.reply_pages.set(Vec::new());
        self.reload_replies();
    }

    pub fn more_replies(self) {
        let cursor = self.reply_cursor.get_untracked();
        if cursor.is_empty() || self.loading_replies.get_untracked() {
            return;
        }
        self.reply_pages.update(|pages| {
            if !pages.contains(&cursor) {
                pages.push(cursor);
            }
        });
        self.reload_replies();
    }

    fn reload_replies(self) {
        self.thread_refresh_id.update(|id| *id = id.wrapping_add(1));
        self.loading_replies.set(true);
        let selected = self.selected.get_untracked();
        leptos::task::spawn_local(async move {
            self.refresh_thread().await;
            if self.selected.get_untracked() == selected {
                self.loading_replies.set(false);
            }
        });
    }

    pub fn open_thread(self, id: String) {
        self.selected.set(id);
        self.thread.set(Value::Null);
        self.thread_from_start.set(false);
        self.reply_pages.set(Vec::new());
        self.reply_cursor.set(String::new());
        self.loading_replies.set(false);
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
