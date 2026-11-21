use leptos::prelude::*;
use serde_json::{Value, json};

use crate::{
    api, content,
    state::Console,
    ui::{Avatar, Icon, Json, StateBadge},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    People,
    Grants,
    Schemas,
    Watchers,
    Sync,
}

#[component]
pub fn Settings() -> impl IntoView {
    let tab = RwSignal::new(Tab::People);
    view! {
        <div class="settings-layout">
            <nav class="settings-nav" aria-label="Settings">
                {[
                    (Tab::People, "People and profiles"),
                    (Tab::Grants, "Access"),
                    (Tab::Schemas, "Schemas"),
                    (Tab::Watchers, "Watchers"),
                    (Tab::Sync, "Sync"),
                ]
                    .into_iter()
                    .map(|(value, label)| {
                        view! {
                            <button
                                class:active=move || tab.get() == value
                                on:click=move |_| tab.set(value)
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect_view()}
            </nav>
            <section class="settings-content">
                {move || match tab.get() {
                    Tab::People => view! { <People /> }.into_any(),
                    Tab::Grants => view! { <Registry kind=RegistryKind::Grants /> }.into_any(),
                    Tab::Schemas => view! { <Registry kind=RegistryKind::Schemas /> }.into_any(),
                    Tab::Watchers => view! { <Registry kind=RegistryKind::Watchers /> }.into_any(),
                    Tab::Sync => view! { <SyncSettings /> }.into_any(),
                }}
            </section>
        </div>
    }
}

#[component]
fn People() -> impl IntoView {
    let console = expect_context::<Console>();
    let people = RwSignal::new(Vec::<Value>::new());
    let theme = RwSignal::new("system".to_owned());
    let profile = RwSignal::new(api::profile());
    leptos::task::spawn_local(async move {
        match api::get("/actors").await {
            Ok(value) => people.set(content::list(&value, "actors")),
            Err(error) => console.report(error),
        }
    });
    view! {
        <div class="settings-heading">
            <h2>"People and profiles"</h2>
            <button
                class="button"
                on:click=move |_| {
                    leptos::task::spawn_local(async move {
                        match api::write("DELETE", "/auth/session", &json!({})).await {
                            Ok(_) => {
                                console.authenticated.set(false);
                                console.claims.update(|claims| claims.clear());
                            }
                            Err(error) => console.report(error),
                        }
                    });
                }
            >
                "Sign out"
            </button>
        </div>
        <div class="preferences">
            <label for="theme">"Appearance"</label>
            <select
                id="theme"
                prop:value=move || theme.get()
                on:change=move |event| {
                    let value = event_target_value(&event);
                    theme.set(value.clone());
                    if let Some(root) = document().document_element() {
                        if value == "system" {
                            let _ = root.remove_attribute("data-theme");
                        } else {
                            let _ = root.set_attribute("data-theme", &value);
                        }
                    }
                }
            >
                <option value="system">"System"</option>
                <option value="light">"Light"</option>
                <option value="dark">"Dark"</option>
            </select>
        </div>
        <form
            class="profile-form"
            on:submit=move |event| {
                event.prevent_default();
                let previous = api::profile();
                api::set_profile(profile.get_untracked().trim().into());
                leptos::task::spawn_local(async move {
                    match api::get("/me").await {
                        Ok(value) => {
                            console.me.set(value);
                            console.claims.update(|claims| claims.clear());
                            console.reload();
                        }
                        Err(error) => {
                            api::set_profile(previous);
                            console.report(error);
                        }
                    }
                });
            }
        >
            <div class="form-row">
                <label for="profile-name">"Act as profile"</label>
                <input
                    id="profile-name"
                    placeholder="Yourself"
                    prop:value=move || profile.get()
                    on:input=move |event| profile.set(event_target_value(&event))
                />
            </div>
            <button type="submit" class="button">
                "Apply"
            </button>
        </form>
        <div class="people-list">
            <For
                each=move || people.get()
                key=|person| content::field(person, "id").to_owned()
                children=move |person| {
                    let name = content::field(&person, "name").to_owned();
                    let handle = content::field(&person, "handle").to_owned();
                    let kind = content::field(&person, "kind").to_owned();
                    view! {
                        <article class="person-row">
                            <Avatar name=name.clone() />
                            <div>
                                <strong>{name}</strong>
                                <span>{handle}</span>
                            </div>
                            <StateBadge state=kind />
                        </article>
                    }
                }
            />
        </div>
        <details class="identity-details">
            <summary>"Your identity and permissions"</summary>
            {move || view! { <Json value=console.me.get() /> }}
        </details>
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegistryKind {
    Grants,
    Schemas,
    Watchers,
}

impl RegistryKind {
    fn path(self) -> &'static str {
        match self {
            Self::Grants => "/grants",
            Self::Schemas => "/schemas",
            Self::Watchers => "/watchers",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Grants => "grants",
            Self::Schemas => "schemas",
            Self::Watchers => "watchers",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Grants => "Access",
            Self::Schemas => "Schemas",
            Self::Watchers => "Watchers",
        }
    }
    fn action(self) -> &'static str {
        match self {
            Self::Grants => "Create grant",
            Self::Schemas => "Register schema",
            Self::Watchers => "Register watchers",
        }
    }
    fn default_body(self) -> Value {
        match self {
            Self::Grants => {
                json!({"principals":[{"kind":"user","userId":""}],"can":{"work":["read","post","react"]}})
            }
            Self::Schemas => {
                json!({"name":"","version":1,"description":"","schema":{"type":"object","properties":{},"additionalProperties":false}})
            }
            Self::Watchers => json!({"url":null,"watch":{"adam":{"tail":"work/"}},"sync":[]}),
        }
    }
}

#[component]
fn Registry(kind: RegistryKind) -> impl IntoView {
    let console = expect_context::<Console>();
    let values = RwSignal::new(Vec::<Value>::new());
    let editing = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let endpoint = RwSignal::new(kind.path().to_owned());
    let method = RwSignal::new("POST".to_owned());
    let load = move || {
        leptos::task::spawn_local(async move {
            match api::get(kind.path()).await {
                Ok(value) => values.set(content::list(&value, kind.key())),
                Err(error) => console.report(error),
            }
        });
    };
    load();
    view! {
        <div class="settings-heading">
            <h2>{kind.title()}</h2>
            <button
                class="button primary"
                on:click=move |_| {
                    draft
                        .set(
                            serde_json::to_string_pretty(&kind.default_body()).unwrap_or_default(),
                        );
                    endpoint.set(kind.path().into());
                    method.set("POST".into());
                    editing.set(true);
                }
            >
                <Icon name="plus" />
                {kind.action()}
            </button>
        </div>
        <Show when=move || editing.get()>
            <form
                class="registry-editor"
                on:submit=move |event| {
                    event.prevent_default();
                    match serde_json::from_str::<Value>(&draft.get_untracked()) {
                        Ok(body) => {
                            leptos::task::spawn_local(async move {
                                if console
                                    .mutate(
                                        &method.get_untracked(),
                                        &endpoint.get_untracked(),
                                        body,
                                    )
                                    .await
                                    .is_ok()
                                {
                                    editing.set(false);
                                    load();
                                }
                            });
                        }
                        Err(error) => console.error.set(format!("Invalid JSON: {error}")),
                    }
                }
            >
                <label for="registry-json">
                    {move || if method.get() == "PATCH" { "Grant changes" } else { kind.action() }}
                </label>
                <textarea
                    id="registry-json"
                    class="code-editor"
                    rows="15"
                    spellcheck="false"
                    prop:value=move || draft.get()
                    on:input=move |event| draft.set(event_target_value(&event))
                />
                <div class="form-actions">
                    <button type="button" class="button" on:click=move |_| editing.set(false)>
                        "Cancel"
                    </button>
                    <button
                        type="submit"
                        class="button primary"
                        disabled=move || console.busy.get()
                    >
                        "Save"
                    </button>
                </div>
            </form>
        </Show>
        <Show
            when=move || !values.get().is_empty()
            fallback=|| view! { <p class="empty">"Nothing registered"</p> }
        >
            <div class="registry-list">
                <For
                    each=move || values.get()
                    key=|value| value.to_string()
                    children=move |value| {
                        let id = StoredValue::new(content::field(&value, "id").to_owned());
                        let title = if kind == RegistryKind::Watchers {
                            content::field(&value, "owner").to_owned()
                        } else {
                            id.get_value()
                        };
                        let built_in = value["builtIn"].as_bool().unwrap_or_default();
                        let entry = StoredValue::new(value);
                        let revoke = RwSignal::new(false);
                        view! {
                            <article class="registry-entry">
                                <div class="registry-entry-heading">
                                    <h3>{title}</h3>
                                    <Show when=move || built_in>
                                        <span class="subtle">"Built in"</span>
                                    </Show>
                                    <Show when=move || kind == RegistryKind::Grants>
                                        <button
                                            class="button small"
                                            on:click={
                                                let id = id.get_value();
                                                move |_| {
                                                    endpoint
                                                        .set(format!("/grants/{}", urlencoding::encode(&id)));
                                                    method.set("PATCH".into());
                                                    draft
                                                        .set(
                                                            serde_json::to_string_pretty(
                                                                    &json!({"can":entry.get_value()["can"]}),
                                                                )
                                                                .unwrap_or_default(),
                                                        );
                                                    editing.set(true);
                                                }
                                            }
                                        >
                                            "Edit"
                                        </button>
                                        <button
                                            class="button small"
                                            on:click=move |_| revoke.set(true)
                                        >
                                            "Revoke"
                                        </button>
                                    </Show>
                                    <Show when=move || kind == RegistryKind::Schemas && !built_in>
                                        <button
                                            class="button small"
                                            on:click=move |_| {
                                                let current = entry.get_value();
                                                draft
                                                    .set(
                                                        serde_json::to_string_pretty(
                                                                &json!(
                                                                    {"name":current["name"],"version":current["version"].as_u64().unwrap_or(0)+1,"description":current["description"],"schema":current["schema"]}
                                                                ),
                                                            )
                                                            .unwrap_or_default(),
                                                    );
                                                endpoint.set("/schemas".into());
                                                method.set("POST".into());
                                                editing.set(true);
                                            }
                                        >
                                            "New version"
                                        </button>
                                    </Show>
                                </div>
                                <Show when=move || {
                                    !content::field(&entry.get_value(), "description").is_empty()
                                }>
                                    <p>
                                        {move || {
                                            content::field(&entry.get_value(), "description").to_owned()
                                        }}
                                    </p>
                                </Show>
                                <details>
                                    <summary>"Definition"</summary>
                                    <Json value=entry.get_value() />
                                </details>
                                <Show when=move || revoke.get()>
                                    <div class="confirm-inline">
                                        <span>"Revoke this grant?"</span>
                                        <button class="button" on:click=move |_| revoke.set(false)>
                                            "Keep"
                                        </button>
                                        <button
                                            class="button danger"
                                            on:click={
                                                let id = id.get_value();
                                                move |_| {
                                                    let id = id.clone();
                                                    leptos::task::spawn_local(async move {
                                                        if console
                                                            .mutate(
                                                                "POST",
                                                                &format!("/grants/{}/revoke", urlencoding::encode(&id)),
                                                                json!({}),
                                                            )
                                                            .await
                                                            .is_ok()
                                                        {
                                                            revoke.set(false);
                                                            load();
                                                        }
                                                    });
                                                }
                                            }
                                        >
                                            "Revoke"
                                        </button>
                                    </div>
                                </Show>
                            </article>
                        }
                    }
                />
            </div>
        </Show>
    }
}

#[component]
fn SyncSettings() -> impl IntoView {
    let console = expect_context::<Console>();
    let entries = RwSignal::new(Vec::<Value>::new());
    let key = RwSignal::new(String::new());
    let target = RwSignal::new(String::new());
    let result = RwSignal::new(Value::Null);
    let mode = RwSignal::new("read".to_owned());
    leptos::task::spawn_local(async move {
        match api::get("/").await {
            Ok(value) => {
                let sync = content::list(&value, "sync");
                if let Some(first) = sync.first() {
                    key.set(content::field(first, "key").into());
                }
                entries.set(sync);
            }
            Err(error) => console.report(error),
        }
    });
    view! {
        <div class="settings-heading">
            <h2>"Sync"</h2>
        </div>
        <div class="sync-entries">
            <For
                each=move || entries.get()
                key=|value| content::field(value, "key").to_owned()
                children=move |value| {
                    view! {
                        <div class="sync-entry">
                            <strong>{content::field(&value, "key").to_owned()}</strong>
                            <StateBadge state=content::field(&value, "platform").to_owned() />
                        </div>
                    }
                }
            />
        </div>
        <form
            class="source-form"
            on:submit=move |event| {
                event.prevent_default();
                let key = urlencoding::encode(&key.get_untracked()).into_owned();
                let target = target.get_untracked();
                let mode = mode.get_untracked();
                leptos::task::spawn_local(async move {
                    let response = match mode.as_str() {
                        "inbox" => api::get(&format!("/inbox/{key}?limit=100")).await,
                        "join" => {
                            api::write(
                                    "POST",
                                    &format!("/sources/{key}/join"),
                                    &json!({"target":target}),
                                )
                                .await
                        }
                        _ => {
                            api::get(
                                    &api::query(
                                        &format!("/sources/{key}/read"),
                                        &[("target", target), ("limit", "100".into())],
                                    ),
                                )
                                .await
                        }
                    };
                    match response {
                        Ok(value) => result.set(value),
                        Err(error) => console.report(error),
                    }
                });
            }
        >
            <div class="form-grid">
                <div class="form-row">
                    <label for="sync-key">"Sync key"</label>
                    <input
                        id="sync-key"
                        required
                        prop:value=move || key.get()
                        on:input=move |event| key.set(event_target_value(&event))
                    />
                </div>
                <div class="form-row">
                    <label for="source-mode">"Action"</label>
                    <select
                        id="source-mode"
                        prop:value=move || mode.get()
                        on:change=move |event| mode.set(event_target_value(&event))
                    >
                        <option value="read">"Read source"</option>
                        <option value="inbox">"Inspect inbox"</option>
                        <option value="join">"Join source channel"</option>
                    </select>
                </div>
            </div>
            <Show when=move || mode.get() != "inbox">
                <div class="form-row">
                    <label for="source-target">"Target"</label>
                    <input
                        id="source-target"
                        required
                        placeholder="#channel or platform thread"
                        prop:value=move || target.get()
                        on:input=move |event| target.set(event_target_value(&event))
                    />
                </div>
            </Show>
            <button class="button" type="submit">
                {move || if mode.get() == "join" { "Join" } else { "Read" }}
            </button>
        </form>
        <Show when=move || {
            !result.get().is_null()
        }>{move || view! { <Json value=result.get() /> }}</Show>
    }
}
