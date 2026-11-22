use leptos::prelude::*;
use serde_json::{Value, json};

use crate::{
    api,
    composer::Composer,
    content,
    settings::Settings,
    state::{Console, Section},
    thread::Thread,
    ui::{Avatar, Icon, StateBadge, timestamp},
};

#[derive(Clone, Copy)]
struct MobileNavigation(RwSignal<bool>);

#[component]
pub fn App() -> impl IntoView {
    let console = Console::new();
    provide_context(console);
    let mobile_nav = RwSignal::new(false);
    provide_context(MobileNavigation(mobile_nav));
    console.boot();
    console.start_live();
    view! {
        <a class="skip-link" href="#workspace">
            "Skip to work"
        </a>
        <Show
            when=move || !console.checking_auth.get()
            fallback=|| {
                view! {
                    <main class="boot" aria-busy="true">
                        "Opening Engage…"
                    </main>
                }
            }
        >
            <Show when=move || console.authenticated.get() fallback=move || view! { <SignIn /> }>
                <div
                    class="console"
                    class:has-thread=move || !console.selected.get().is_empty()
                    class:nav-open=move || mobile_nav.get()
                >
                    <aside class="sidebar" aria-label="Board navigation">
                        <div class="brand">
                            <span class="brand-mark" aria-hidden="true">
                                <i />
                                <i />
                            </span>
                            <span>"Engage"</span>
                            <button
                                class="icon-button mobile-only"
                                aria-label="Close navigation"
                                on:click=move |_| mobile_nav.set(false)
                            >
                                <Icon name="close" />
                            </button>
                        </div>
                        <nav class="primary-nav" aria-label="Work views">
                            <Nav
                                section=Section::Conversations
                                title="Conversations"
                                icon="channel"
                            />
                            <Nav section=Section::Tasks title="Tasks" icon="check" />
                            <Nav section=Section::Activity title="Activity" icon="clock" />
                            <Nav section=Section::Schedules title="Schedules" icon="clock" />
                        </nav>
                        <div class="channel-heading">
                            <h2>"Channels"</h2>
                            <NewChannel />
                        </div>
                        <nav class="channels" aria-label="Channels">
                            <For
                                each=move || console.channels.get()
                                key=|value| content::field(value, "path").to_owned()
                                children=move |value| {
                                    let channel = content::field(&value, "path").to_owned();
                                    let active = channel.clone();
                                    let label = channel.clone();
                                    view! {
                                        <button
                                            class="channel-row"
                                            class:active=move || console.channel.get() == active
                                            on:click=move |_| {
                                                console.select_channel(channel.clone());
                                                mobile_nav.set(false);
                                            }
                                        >
                                            <Icon name="channel" />
                                            <span>{label}</span>
                                        </button>
                                    }
                                }
                            />
                        </nav>
                        <div class="sidebar-bottom">
                            <Nav section=Section::Settings title="Settings" icon="settings" />
                            <div class="identity">
                                {move || {
                                    view! {
                                        <Avatar name=content::field(&console.me.get(), "name")
                                            .to_owned() />
                                    }
                                }}
                                <span>
                                    {move || content::field(&console.me.get(), "name").to_owned()}
                                </span>
                                <button
                                    class="icon-button"
                                    aria-label="Refresh board"
                                    on:click=move |_| console.reload()
                                >
                                    <Icon name="refresh" />
                                </button>
                            </div>
                        </div>
                    </aside>
                    <main id="workspace" class="workspace">
                        <header class="workspace-header">
                            <button
                                class="icon-button mobile-only"
                                aria-label="Open navigation"
                                on:click=move |_| mobile_nav.set(true)
                            >
                                <Icon name="menu" />
                            </button>
                            <h1>
                                {move || {
                                    if console.section.get() == Section::Settings {
                                        "Settings".into()
                                    } else if console.channel.get().is_empty() {
                                        "All channels".into()
                                    } else {
                                        console.channel.get()
                                    }
                                }}
                            </h1>
                            <span class="header-view">
                                {move || match console.section.get() {
                                    Section::Conversations => "Conversations",
                                    Section::Tasks => "Tasks",
                                    Section::Activity => "Activity",
                                    Section::Schedules => "Schedules",
                                    Section::Settings => "",
                                }}
                            </span>
                            <Show when=move || {
                                matches!(
                                    console.section.get(),
                                    Section::Conversations | Section::Tasks | Section::Schedules
                                )
                            }>
                                <button
                                    class="button primary"
                                    on:click=move |_| {
                                        console.selected.set(String::new());
                                        console.composing.set(true);
                                    }
                                >
                                    <Icon name="plus" />
                                    {move || match console.section.get() {
                                        Section::Tasks => "New task",
                                        Section::Schedules => "Schedule",
                                        _ => "New thread",
                                    }}
                                </button>
                            </Show>
                        </header>
                        <Show when=move || !console.error.get().is_empty()>
                            <div class="error-bar" role="alert">
                                <span>{move || console.error.get()}</span>
                                <button
                                    class="icon-button"
                                    aria-label="Dismiss error"
                                    on:click=move |_| console.error.set(String::new())
                                >
                                    <Icon name="close" />
                                </button>
                            </div>
                        </Show>
                        {move || {
                            if console.section.get() == Section::Settings {
                                view! { <Settings /> }.into_any()
                            } else {
                                view! {
                                    <div class="work-panes">
                                        <section class="feed" aria-label="Work list">
                                            <FeedTools />
                                            <Show
                                                when=move || !console.loading.get()
                                                fallback=|| {
                                                    view! {
                                                        <p class="empty" aria-busy="true">
                                                            "Loading…"
                                                        </p>
                                                    }
                                                }
                                            >
                                                <Show
                                                    when=move || !console.items.get().is_empty()
                                                    fallback=move || view! { <Empty /> }
                                                >
                                                    <div class="feed-list">
                                                        <For
                                                            each=move || console.items.get()
                                                            key=content::row_key
                                                            children=move |value| view! { <FeedItem value /> }
                                                        />
                                                    </div>
                                                </Show>
                                                <Show when=move || {
                                                    !console.older.get().is_empty()
                                                        && matches!(console.section.get(), Section::Conversations | Section::Tasks)
                                                }>
                                                    <button
                                                        class="button load-more"
                                                        disabled=move || console.loading_more.get()
                                                        on:click=move |_| console.load_more()
                                                    >
                                                        {move || if console.loading_more.get() { "Loading…" } else if console.section.get() == Section::Tasks { "More tasks" } else { "Older conversations" }}
                                                    </button>
                                                </Show>
                                            </Show>
                                        </section>
                                        <Show
                                            when=move || console.composing.get()
                                            fallback=move || view! { <Thread /> }
                                        >
                                            <section class="thread-pane">
                                                <div class="pane-heading">
                                                    <h2>
                                                        {move || match console.section.get() {
                                                            Section::Tasks => "New task",
                                                            Section::Schedules => "Schedule message",
                                                            _ => "New thread",
                                                        }}
                                                    </h2>
                                                    <button
                                                        class="icon-button"
                                                        aria-label="Close composer"
                                                        on:click=move |_| console.composing.set(false)
                                                    >
                                                        <Icon name="close" />
                                                    </button>
                                                </div>
                                                <Composer reply_to=None />
                                            </section>
                                        </Show>
                                    </div>
                                }
                                    .into_any()
                            }
                        }}
                    </main>
                </div>
            </Show>
        </Show>
    }
}

#[component]
fn Nav(section: Section, title: &'static str, icon: &'static str) -> impl IntoView {
    let console = expect_context::<Console>();
    let navigation = expect_context::<MobileNavigation>();
    view! {
        <button
            class="nav-row"
            class:active=move || console.section.get() == section
            on:click=move |_| {
                console.select_section(section);
                navigation.0.set(false);
            }
        >
            <Icon name=icon />
            <span>{title}</span>
        </button>
    }
}

#[component]
fn NewChannel() -> impl IntoView {
    let console = expect_context::<Console>();
    let open = RwSignal::new(false);
    let path = RwSignal::new(String::new());
    view! {
        <button
            class="icon-button"
            aria-label="New channel"
            on:click=move |_| open.update(|value| *value = !*value)
        >
            <Icon name="plus" />
        </button>
        <Show when=move || open.get()>
            <form
                class="new-channel"
                on:submit=move |event| {
                    event.prevent_default();
                    console.new_channel(path.get_untracked());
                    path.set(String::new());
                    open.set(false);
                }
            >
                <label for="channel-path">"Channel path"</label>
                <input
                    id="channel-path"
                    required
                    placeholder="work/reviews"
                    prop:value=move || path.get()
                    on:input=move |event| path.set(event_target_value(&event))
                />
                <button class="button primary" type="submit">
                    "Create"
                </button>
            </form>
        </Show>
    }
}

#[component]
fn FeedTools() -> impl IntoView {
    let console = expect_context::<Console>();
    view! {
        <div class="feed-controls">
            <div class="feed-tools">
                <Show when=move || console.section.get() == Section::Conversations>
                    <form
                        class="search"
                        on:submit=move |event| {
                            event.prevent_default();
                            console.pages.set(Vec::new());
                            console.reload();
                        }
                    >
                        <Icon name="search" />
                        <input
                            aria-label="Search conversations"
                            placeholder="Search conversations"
                            prop:value=move || console.search.get()
                            on:input=move |event| console.search.set(event_target_value(&event))
                        />
                        <button type="submit" class="visually-hidden">
                            "Search"
                        </button>
                    </form>
                </Show>
                <Show when=move || console.section.get() == Section::Tasks>
                    <label class="visually-hidden" for="task-state">
                        "Task state"
                    </label>
                    <select
                        id="task-state"
                        prop:value=move || console.task_filter.get()
                        on:change=move |event| {
                            console.task_filter.set(event_target_value(&event));
                            console.pages.set(Vec::new());
                            console.reload();
                        }
                    >
                        <option value="open">"Open tasks"</option>
                        <option value="ready">"Ready"</option>
                        <option value="claimed">"Claimed"</option>
                        <option value="blocked">"Blocked"</option>
                        <option value="waiting">"Waiting"</option>
                        <option value="completed">"Completed"</option>
                        <option value="failed">"Failed"</option>
                        <option value="canceled">"Canceled"</option>
                        <option value="all">"All tasks"</option>
                    </select>
                </Show>
                <Show when=move || {
                    matches!(console.section.get(), Section::Activity | Section::Schedules)
                }>
                    <span class="subtle">
                        {move || {
                            if console.section.get() == Section::Activity {
                                "Latest activity"
                            } else {
                                "Scheduled messages"
                            }
                        }}
                    </span>
                </Show>
            </div>
            <Show when=move || console.section.get() != Section::Schedules>
                <details class="feed-filters">
                    <summary>"Filters"</summary>
                    <form on:submit=move |event| {
                        event.prevent_default();
                        console.pages.set(Vec::new());
                        console.reload();
                    }>
                        <div class="form-row">
                            <label for="filter-by">"Author"</label>
                            <input
                                id="filter-by"
                                placeholder="Handle or actor ID"
                                prop:value=move || console.by.get()
                                on:input=move |event| console.by.set(event_target_value(&event))
                            />
                        </div>
                        <div class="form-row">
                            <label for="filter-labels">"Labels"</label>
                            <input
                                id="filter-labels"
                                placeholder="priority=high, status=open"
                                prop:value=move || console.labels.get()
                                on:input=move |event| console.labels.set(event_target_value(&event))
                            />
                        </div>
                        <Show when=move || console.section.get() != Section::Tasks>
                            <div class="form-row">
                                <label for="filter-source">"Source"</label>
                                <input
                                    id="filter-source"
                                    placeholder="board, slack, github…"
                                    prop:value=move || console.source.get()
                                    on:input=move |event| {
                                        console.source.set(event_target_value(&event))
                                    }
                                />
                            </div>
                        </Show>
                        <div class="form-actions">
                            <button
                                class="button small"
                                type="button"
                                on:click=move |_| {
                                    console.by.set(String::new());
                                    console.labels.set(String::new());
                                    console.source.set(String::new());
                                    console.pages.set(Vec::new());
                                    console.reload();
                                }
                            >
                                "Clear"
                            </button>
                            <button class="button primary small" type="submit">
                                "Apply"
                            </button>
                        </div>
                    </form>
                </details>
            </Show>
        </div>
    }
}

#[component]
fn Empty() -> impl IntoView {
    let console = expect_context::<Console>();
    view! {
        <div class="empty">
            <h2>
                {move || match console.section.get() {
                    Section::Tasks => "No tasks",
                    Section::Activity => "No activity",
                    Section::Schedules => "No schedules",
                    _ => "No conversations",
                }}
            </h2>
            <Show when=move || console.section.get() != Section::Activity>
                <button class="button" on:click=move |_| console.composing.set(true)>
                    <Icon name="plus" />
                    {move || {
                        if console.section.get() == Section::Tasks {
                            "Create task"
                        } else {
                            "Write a message"
                        }
                    }}
                </button>
            </Show>
        </div>
    }
}

#[component]
fn FeedItem(value: Value) -> impl IntoView {
    let console = expect_context::<Console>();
    match console.section.get_untracked() {
        Section::Activity => {
            let kind = content::field(&value, "type").to_owned();
            let time = timestamp(content::field(&value, "createdAt"));
            let message = value.get("message").cloned().unwrap_or(Value::Null);
            let id = content::field(&message, "id").to_owned();
            let summary = content::title(content::field(&message, "text"));
            view! {
                <button
                    class="feed-row activity-row"
                    disabled=id.is_empty()
                    on:click=move |_| console.open_thread(id.clone())
                >
                    <span class="activity-dot" />
                    <div>
                        <span class="row-title">{kind}</span>
                        <p>{summary}</p>
                        <time>{time}</time>
                    </div>
                </button>
            }
            .into_any()
        }
        Section::Schedules => {
            let id = content::field(&value, "id").to_owned();
            let text = content::title(content::field(&value, "text"));
            let state = content::field(&value, "state").to_owned();
            let due = content::field(&value, "next").to_owned();
            let pending = state == "pending";
            let id = StoredValue::new(id);
            view! {
                <article class="feed-row schedule-row">
                    <div class="row-top">
                        <StateBadge state />
                        <time>{timestamp(&due)}</time>
                    </div>
                    <p class="row-title">{text}</p>
                    <Show when=move || pending>
                        <button
                            class="button small"
                            on:click=move |_| {
                                let id = id.get_value();
                                leptos::task::spawn_local(async move {
                                    let _ = console
                                        .mutate(
                                            "POST",
                                            &format!("/schedules/{}/cancel", urlencoding::encode(&id)),
                                            json!({}),
                                        )
                                        .await;
                                });
                            }
                        >
                            "Cancel schedule"
                        </button>
                    </Show>
                </article>
            }.into_any()
        }
        _ => {
            let message = content::message(&value).clone();
            let id = content::field(&message, "id").to_owned();
            let active = id.clone();
            let title = content::title(content::field(&message, "text"));
            let author = content::author(&message);
            let created = timestamp(content::field(&message, "createdAt"));
            let state = content::task_state(&message).to_owned();
            let has_task = !state.is_empty();
            let replies = value
                .get("replies")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            view! {
                <button
                    class="feed-row"
                    class:selected=move || console.selected.get() == active
                    on:click=move |_| console.open_thread(id.clone())
                >
                    <div class="row-top">
                        <span>{author}</span>
                        <time>{created}</time>
                    </div>
                    <p class="row-title">{title}</p>
                    <div class="row-bottom">
                        <Show when=move || has_task>
                            <StateBadge state=state.clone() />
                        </Show>
                        <Show when=move || { replies > 0 }>
                            <span class="reply-count">
                                {format!(
                                    "{replies} {}",
                                    if replies == 1 { "reply" } else { "replies" },
                                )}
                            </span>
                        </Show>
                    </div>
                </button>
            }
            .into_any()
        }
    }
}

#[component]
fn SignIn() -> impl IntoView {
    let console = expect_context::<Console>();
    let token = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    view! {
        <main class="signin">
            <Show
                when=|| !api::is_proxy()
                fallback=|| {
                    view! {
                        <section class="session-ended">
                            <span class="brand">"Engage"</span>
                            <h1>"Session ended"</h1>
                            <p>"Run "<code>"engage inspect"</code>" again to reconnect."</p>
                        </section>
                    }
                }
            >
                <form on:submit=move |event| {
                    event.prevent_default();
                    busy.set(true);
                    let secret = token.get_untracked();
                    token.set(String::new());
                    leptos::task::spawn_local(async move {
                        match api::sign_in(&secret).await {
                            Ok(()) => {
                                console.error.set(String::new());
                                console.boot();
                            }
                            Err(error) => console.report(error),
                        }
                        busy.set(false);
                    });
                }>
                    <span class="brand">"Engage"</span>
                    <h1>"Sign in"</h1>
                    <label for="access-token">"Access token"</label>
                    <input
                        id="access-token"
                        type="password"
                        autocomplete="off"
                        required
                        prop:value=move || token.get()
                        on:input=move |event| token.set(event_target_value(&event))
                    />
                    <Show when=move || !console.error.get().is_empty()>
                        <p role="alert" class="form-error">
                            {move || console.error.get()}
                        </p>
                    </Show>
                    <button class="button primary" type="submit" disabled=move || busy.get()>
                        {move || if busy.get() { "Signing in…" } else { "Sign in" }}
                    </button>
                </form>
            </Show>
        </main>
    }
}
