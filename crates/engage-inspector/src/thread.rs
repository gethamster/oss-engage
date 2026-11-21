use leptos::prelude::*;
use serde_json::{Value, json};

use crate::{
    composer::Composer,
    content,
    state::Console,
    ui::{Avatar, Icon, Markdown, StateBadge, timestamp},
};

#[component]
pub fn Thread() -> impl IntoView {
    let console = expect_context::<Console>();
    view! {
        <section class="thread-pane" aria-label="Conversation">
            <Show
                when=move || !console.selected.get().is_empty()
                fallback=|| {
                    view! {
                        <div class="thread-placeholder">
                            <span class="placeholder-mark" aria-hidden="true" />
                            <h2>"Select a conversation"</h2>
                        </div>
                    }
                }
            >
                <div class="pane-heading">
                    <button
                        class="icon-button mobile-back"
                        aria-label="Back to conversations"
                        on:click=move |_| console.select_channel(console.channel.get_untracked())
                    >
                        <Icon name="back" />
                    </button>
                    <h2>
                        {move || content::title(
                            content::field(content::message(&console.thread.get()), "text"),
                        )}
                    </h2>
                    <button
                        class="icon-button"
                        aria-label="Close conversation"
                        on:click=move |_| console.select_channel(console.channel.get_untracked())
                    >
                        <Icon name="close" />
                    </button>
                </div>
                <Show
                    when=move || !console.thread.get().is_null()
                    fallback=|| {
                        view! {
                            <p class="empty" aria-busy="true">
                                "Loading conversation…"
                            </p>
                        }
                    }
                >
                    <div class="thread-content">
                        <TaskBar />
                        <For
                            each=move || {
                                let thread = console.thread.get();
                                let root = content::message(&thread);
                                let mut ids = vec![content::field(root, "id").to_owned()];
                                ids.extend(
                                    content::list(&thread, "replies")
                                        .iter()
                                        .map(|message| content::field(message, "id").to_owned()),
                                );
                                ids.into_iter().filter(|id| !id.is_empty()).collect::<Vec<_>>()
                            }
                            key=|id| id.clone()
                            children=move |id| view! { <MessageCard id /> }
                        />
                        {move || {
                            let selected = console.selected.get();
                            view! { <Composer reply_to=Some(selected) /> }
                        }}
                    </div>
                </Show>
            </Show>
        </section>
    }
}

#[component]
fn MessageCard(id: String) -> impl IntoView {
    let console = expect_context::<Console>();
    let id = StoredValue::new(id);
    let message = Signal::derive(move || {
        let thread = console.thread.get();
        let root = content::message(&thread);
        if content::field(root, "id") == id.get_value() {
            return root.clone();
        }
        content::list(&thread, "replies")
            .into_iter()
            .find(|message| content::field(message, "id") == id.get_value())
            .unwrap_or(Value::Null)
    });
    let editing = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let revision = RwSignal::new(Value::Null);
    let confirm_delete = RwSignal::new(false);
    let emoji = RwSignal::new(String::new());
    let label_key = RwSignal::new(String::new());
    let label_value = RwSignal::new(String::new());
    let adding_label = RwSignal::new(false);
    let adding_reaction = RwSignal::new(false);
    view! {
        <article
            class="message-card"
            class:deleted=move || message.get()["deleted"].as_bool().unwrap_or_default()
        >
            <header class="message-header">
                {move || view! { <Avatar name=content::author(&message.get()) /> }}<div>
                    <strong>{move || content::author(&message.get())}</strong>
                    <div class="message-meta">
                        <time datetime=move || {
                            content::field(&message.get(), "createdAt").to_owned()
                        }>{move || timestamp(content::field(&message.get(), "createdAt"))}</time>
                        <Show when=move || !message.get()["editedAt"].is_null()>
                            <span>"edited"</span>
                        </Show>
                        <Show when=move || !message.get()["from"].is_null()>
                            <span>{move || content::field(&message.get(), "from").to_owned()}</span>
                        </Show>
                    </div>
                </div><details class="message-menu">
                    <summary aria-label="Message actions">
                        <Icon name="menu" />
                    </summary>
                    <div class="menu">
                        <button on:click=move |_| {
                            draft.set(content::field(&message.get_untracked(), "text").into());
                            revision.set(message.get_untracked()["revision"].clone());
                            editing.set(true);
                        }>"Edit"</button>
                        <button on:click=move |_| {
                            revision.set(message.get_untracked()["revision"].clone());
                            confirm_delete.set(true);
                        }>"Delete"</button>
                        <button on:click=move |_| adding_label.set(true)>"Add label"</button>
                        <button on:click=move |_| adding_reaction.set(true)>"Add reaction"</button>
                    </div>
                </details>
            </header>
            <Show
                when=move || !message.get()["deleted"].as_bool().unwrap_or_default()
                fallback=|| view! { <p class="deleted-text">"Message deleted"</p> }
            >
                <Show
                    when=move || !editing.get()
                    fallback=move || {
                        view! {
                            <form
                                class="edit-form"
                                on:submit=move |event| {
                                    event.prevent_default();
                                    let body = json!(
                                        {"text":draft.get_untracked(),"revision":revision.get_untracked()}
                                    );
                                    let path = format!(
                                        "/messages/{}/edit",
                                        urlencoding::encode(&id.get_value()),
                                    );
                                    leptos::task::spawn_local(async move {
                                        if console.mutate("POST", &path, body).await.is_ok() {
                                            editing.set(false);
                                        }
                                    });
                                }
                            >
                                <label
                                    class="visually-hidden"
                                    for=format!("edit-{}", id.get_value())
                                >
                                    "Message"
                                </label>
                                <textarea
                                    id=format!("edit-{}", id.get_value())
                                    rows="6"
                                    prop:value=move || draft.get()
                                    on:input=move |event| draft.set(event_target_value(&event))
                                />
                                <div class="form-actions">
                                    <button
                                        class="button"
                                        type="button"
                                        on:click=move |_| editing.set(false)
                                    >
                                        "Cancel"
                                    </button>
                                    <button
                                        class="button primary"
                                        type="submit"
                                        disabled=move || console.busy.get()
                                    >
                                        "Save"
                                    </button>
                                </div>
                            </form>
                        }
                    }
                >
                    {move || {
                        view! {
                            <Markdown text=content::field(&message.get(), "text").to_owned() />
                        }
                    }}
                </Show>
                <Show when=move || !content::list(&message.get(), "attachments").is_empty()>
                    <div class="message-attachments">
                        <For
                            each=move || content::list(&message.get(), "attachments")
                            key=|file| content::field(file, "id").to_owned()
                            children=move |file| {
                                let href = crate::api::url(
                                    &format!(
                                        "/attachments/{}",
                                        urlencoding::encode(content::field(&file, "id")),
                                    ),
                                );
                                let name = content::field(&file, "name").to_owned();
                                view! {
                                    <a class="attachment" href=href download=name.clone()>
                                        <Icon name="attach" />
                                        <span>{name.clone()}</span>
                                    </a>
                                }
                            }
                        />
                    </div>
                </Show>
                <div class="message-labels">
                    <For
                        each=move || {
                            message
                                .get()["labels"]
                                .as_object()
                                .map(|labels| {
                                    labels
                                        .iter()
                                        .filter(|(key, _)| key.as_str() != "board.task")
                                        .map(|(key, value)| (
                                            key.clone(),
                                            value.as_str().unwrap_or_default().to_owned(),
                                        ))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        }
                        key=|(key, value)| format!("{key}={value}")
                        children=move |(key, value)| {
                            let reserved = key.starts_with("board.");
                            let label = if value.is_empty() {
                                key.clone()
                            } else {
                                format!("{key}={value}")
                            };
                            let key = StoredValue::new(key);
                            let value = StoredValue::new(value);
                            view! {
                                <span class="label-chip">
                                    {label}<Show when=move || !reserved>
                                        <button
                                            class="chip-remove"
                                            aria-label=format!("Remove label {}", key.get_value())
                                            on:click=move |_| {
                                                let path = format!(
                                                    "/messages/{}/labels",
                                                    urlencoding::encode(&id.get_value()),
                                                );
                                                let body = json!(
                                                    {"unset":[key.get_value()],"if":{key.get_value():value.get_value()}}
                                                );
                                                leptos::task::spawn_local(async move {
                                                    let _ = console.mutate("POST", &path, body).await;
                                                });
                                            }
                                        >
                                            <Icon name="close" />
                                        </button>
                                    </Show>
                                </span>
                            }
                        }
                    />
                </div>
                <div class="reactions">
                    <For
                        each=move || {
                            message
                                .get()["reactions"]
                                .as_object()
                                .map(|reactions| {
                                    reactions
                                        .iter()
                                        .map(|(emoji, actors)| (
                                            emoji.clone(),
                                            actors.as_array().map(Vec::len).unwrap_or_default(),
                                        ))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        }
                        key=|(emoji, count)| format!("{emoji}:{count}")
                        children=move |(emoji, count)| {
                            let label = format!("{emoji} {count}");
                            view! {
                                <button
                                    class="reaction"
                                    on:click=move |_| {
                                        let path = format!(
                                            "/messages/{}/reactions",
                                            urlencoding::encode(&id.get_value()),
                                        );
                                        let actor = console.me.get_untracked()["actor"].clone();
                                        let remove = message
                                            .get_untracked()["reactions"][&emoji]
                                            .as_array()
                                            .is_some_and(|actors| actors.contains(&actor));
                                        let body = json!({"emoji":emoji,"remove":remove});
                                        leptos::task::spawn_local(async move {
                                            let _ = console.mutate("POST", &path, body).await;
                                        });
                                    }
                                >
                                    {label}
                                </button>
                            }
                        }
                    />
                </div>
                <Show when=move || adding_label.get()>
                    <form
                        class="inline-form"
                        on:submit=move |event| {
                            event.prevent_default();
                            let path = format!(
                                "/messages/{}/labels",
                                urlencoding::encode(&id.get_value()),
                            );
                            let body = json!(
                                {"set":{label_key.get_untracked():label_value.get_untracked()}}
                            );
                            leptos::task::spawn_local(async move {
                                if console.mutate("POST", &path, body).await.is_ok() {
                                    adding_label.set(false);
                                    label_key.set(String::new());
                                    label_value.set(String::new());
                                }
                            });
                        }
                    >
                        <input
                            aria-label="Label key"
                            required
                            placeholder="priority"
                            prop:value=move || label_key.get()
                            on:input=move |event| label_key.set(event_target_value(&event))
                        />
                        <input
                            aria-label="Label value"
                            placeholder="high"
                            prop:value=move || label_value.get()
                            on:input=move |event| label_value.set(event_target_value(&event))
                        />
                        <button type="submit" class="button">
                            "Add"
                        </button>
                        <button
                            type="button"
                            class="icon-button"
                            aria-label="Cancel label"
                            on:click=move |_| adding_label.set(false)
                        >
                            <Icon name="close" />
                        </button>
                    </form>
                </Show>
                <Show when=move || adding_reaction.get()>
                    <form
                        class="inline-form"
                        on:submit=move |event| {
                            event.prevent_default();
                            let path = format!(
                                "/messages/{}/reactions",
                                urlencoding::encode(&id.get_value()),
                            );
                            let body = json!({"emoji":emoji.get_untracked()});
                            leptos::task::spawn_local(async move {
                                if console.mutate("POST", &path, body).await.is_ok() {
                                    adding_reaction.set(false);
                                    emoji.set(String::new());
                                }
                            });
                        }
                    >
                        <input
                            aria-label="Reaction"
                            required
                            placeholder="Emoji or :name:"
                            prop:value=move || emoji.get()
                            on:input=move |event| emoji.set(event_target_value(&event))
                        />
                        <button type="submit" class="button">
                            "React"
                        </button>
                        <button
                            type="button"
                            class="icon-button"
                            aria-label="Cancel reaction"
                            on:click=move |_| adding_reaction.set(false)
                        >
                            <Icon name="close" />
                        </button>
                    </form>
                </Show>
            </Show>
            <Show when=move || confirm_delete.get()>
                <div class="confirm-inline" role="group" aria-label="Delete message confirmation">
                    <span>"Delete this message?"</span>
                    <button class="button" on:click=move |_| confirm_delete.set(false)>
                        "Keep"
                    </button>
                    <button
                        class="button danger"
                        disabled=move || console.busy.get()
                        on:click=move |_| {
                            let path = format!(
                                "/messages/{}/delete",
                                urlencoding::encode(&id.get_value()),
                            );
                            let body = json!({"revision":revision.get_untracked()});
                            leptos::task::spawn_local(async move {
                                if console.mutate("POST", &path, body).await.is_ok() {
                                    confirm_delete.set(false);
                                }
                            });
                        }
                    >
                        "Delete"
                    </button>
                </div>
            </Show>
        </article>
    }
}

#[component]
fn TaskBar() -> impl IntoView {
    let console = expect_context::<Console>();
    let root = Signal::derive(move || content::message(&console.thread.get()).clone());
    let action = RwSignal::new(String::new());
    let note = RwSignal::new(String::new());
    let task = Signal::derive(move || root.get()["task"].clone());
    let state = Signal::derive(move || content::field(&task.get(), "state").to_owned());
    let id = Signal::derive(move || content::field(&root.get(), "id").to_owned());
    let has_claim = Signal::derive(move || console.claims.get().contains_key(&id.get()));
    let target = RwSignal::new(String::new());
    let sync_open = RwSignal::new(false);
    view! {
        <Show
            when=move || !task.get().is_null()
            fallback=move || {
                view! {
                    <div class="thread-tools">
                        <button
                            class="button small"
                            disabled=move || console.busy.get()
                            on:click=move |_| {
                                let re = id.get_untracked();
                                leptos::task::spawn_local(async move {
                                    let _ = console
                                        .mutate("POST", "/tasks", json!({"re":re}))
                                        .await;
                                });
                            }
                        >
                            "Make task"
                        </button>
                        <button
                            class="button small"
                            on:click=move |_| sync_open.update(|open| *open = !*open)
                        >
                            <Icon name="link" />
                            "Sync"
                        </button>
                    </div>
                }
            }
        >
            <section class="task-panel" aria-label="Task">
                <div class="task-top">
                    {move || view! { <StateBadge state=state.get() /> }}
                    <Show when=move || !task.get()["for"].is_null()>
                        <span class="subtle">
                            {move || format!("Assigned to {}", content::field(&task.get(), "for"))}
                        </span>
                    </Show><Show when=move || !task.get()["claim"].is_null()>
                        <span class="subtle">
                            {move || {
                                format!(
                                    "{} · until {}",
                                    content::field(&task.get()["claim"], "actor"),
                                    timestamp(content::field(&task.get()["claim"], "until")),
                                )
                            }}
                        </span>
                    </Show>
                </div>
                <Show when=move || {
                    !task.get()["result"].is_null()
                }>
                    {move || {
                        view! { <Markdown text=content::field(&task.get(), "result").to_owned() /> }
                    }}
                </Show>
                <Show when=move || !task.get()["blocked"].is_null()>
                    <p>{move || content::field(&task.get()["blocked"], "reason").to_owned()}</p>
                </Show>
                <Show when=move || !task.get()["reason"].is_null()>
                    <p>{move || content::field(&task.get(), "reason").to_owned()}</p>
                </Show>
                <Show when=move || !content::list(&task.get(), "needs").is_empty()>
                    <div class="task-needs">
                        <span class="subtle">"Depends on"</span>
                        <For
                            each=move || content::list(&task.get(), "needs")
                            key=|value| value.to_string()
                            children=move |value| {
                                let dep = value.as_str().unwrap_or_default().to_owned();
                                let label = dep.clone();
                                view! {
                                    <button
                                        class="text-button"
                                        on:click=move |_| console.open_thread(dep.clone())
                                    >
                                        {label}
                                    </button>
                                }
                            }
                        />
                    </div>
                </Show>
                <div class="task-actions">
                    <Show when=move || state.get() == "ready">
                        <button
                            class="button primary small"
                            disabled=move || console.busy.get()
                            on:click=move |_| {
                                console.task_action(id.get_untracked(), "claim", json!({}))
                            }
                        >
                            "Claim"
                        </button>
                    </Show>
                    <Show when=move || state.get() == "claimed" && has_claim.get()>
                        <button
                            class="button primary small"
                            on:click=move |_| {
                                note.set(String::new());
                                action.set("complete".into());
                            }
                        >
                            "Complete"
                        </button>
                        <button
                            class="button small"
                            on:click=move |_| {
                                console.task_action(id.get_untracked(), "renew", json!({}))
                            }
                        >
                            "Renew"
                        </button>
                        <button
                            class="button small"
                            on:click=move |_| {
                                console.task_action(id.get_untracked(), "release", json!({}))
                            }
                        >
                            "Release"
                        </button>
                        <button
                            class="button small"
                            on:click=move |_| {
                                note.set(String::new());
                                action.set("fail".into());
                            }
                        >
                            "Fail"
                        </button>
                    </Show>
                    <Show when=move || {
                        matches!(state.get().as_str(), "ready" | "waiting") || has_claim.get()
                    }>
                        <button
                            class="button small"
                            on:click=move |_| {
                                note.set(String::new());
                                action.set("block".into());
                            }
                        >
                            "Hold"
                        </button>
                    </Show>
                    <Show when=move || state.get() == "blocked">
                        <button
                            class="button small"
                            disabled=move || console.busy.get()
                            on:click=move |_| {
                                console.task_action(id.get_untracked(), "unblock", json!({}))
                            }
                        >
                            "Unblock"
                        </button>
                    </Show>
                    <Show when=move || {
                        matches!(state.get().as_str(), "completed" | "failed" | "canceled")
                    }>
                        <button
                            class="button small"
                            disabled=move || console.busy.get()
                            on:click=move |_| {
                                console.task_action(id.get_untracked(), "reopen", json!({}))
                            }
                        >
                            "Reopen"
                        </button>
                    </Show>
                    <details class="task-menu">
                        <summary class="button small">"More"</summary>
                        <div class="menu">
                            <button on:click=move |_| {
                                note.set(content::field(&task.get_untracked(), "for").into());
                                action.set("assign".into());
                            }>"Assign"</button>
                            <button on:click=move |_| {
                                note.set(
                                    content::list(&task.get_untracked(), "needs")
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                );
                                action.set("needs".into());
                            }>"Dependencies"</button>
                            <button on:click=move |_| {
                                note.set(String::new());
                                action.set("cancel".into());
                            }>"Cancel task"</button>
                            <button on:click=move |_| {
                                sync_open.update(|open| *open = !*open)
                            }>"Sync thread"</button>
                        </div>
                    </details>
                </div>
                <Show when=move || !action.get().is_empty()>
                    <form
                        class="task-action-form"
                        on:submit=move |event| {
                            event.prevent_default();
                            let action_name = action.get_untracked();
                            let note = note.get_untracked();
                            let body = match action_name.as_str() {
                                "complete" => json!({"result":note}),
                                "assign" => {
                                    json!(
                                        {"for":if note.trim().is_empty(){Value::Null}else{note.trim().into()}}
                                    )
                                }
                                "needs" => {
                                    json!(
                                        {"needs":note.split([',',' ']).map(str::trim).filter(|value|!value.is_empty()).collect::<Vec<_>>()}
                                    )
                                }
                                _ => json!({"reason":note}),
                            };
                            console.task_action(id.get_untracked(), &action_name, body);
                            action.set(String::new());
                        }
                    >
                        <label for="task-note">
                            {move || match action.get().as_str() {
                                "complete" => "Result and proof",
                                "assign" => "Actor ID (blank to unassign)",
                                "needs" => "Task IDs, separated by commas",
                                _ => "Reason",
                            }}
                        </label>
                        <textarea
                            id="task-note"
                            rows="3"
                            required=move || {
                                matches!(action.get().as_str(), "fail" | "block" | "cancel")
                            }
                            prop:value=move || note.get()
                            on:input=move |event| note.set(event_target_value(&event))
                        />
                        <div class="form-actions">
                            <button
                                class="button"
                                type="button"
                                on:click=move |_| action.set(String::new())
                            >
                                "Keep editing"
                            </button>
                            <button
                                class="button primary"
                                type="submit"
                                disabled=move || console.busy.get()
                            >
                                {move || match action.get().as_str() {
                                    "complete" => "Complete",
                                    "assign" | "needs" => "Save",
                                    "block" => "Hold",
                                    "fail" => "Fail",
                                    _ => "Cancel task",
                                }}
                            </button>
                        </div>
                    </form>
                </Show>
            </section>
        </Show>
        <Show when=move || sync_open.get()>
            <form
                class="sync-form inline-form"
                on:submit=move |event| {
                    event.prevent_default();
                    let path = format!(
                        "/messages/{}/sync",
                        urlencoding::encode(&id.get_untracked()),
                    );
                    let body = json!({"target":target.get_untracked()});
                    leptos::task::spawn_local(async move {
                        if console.mutate("POST", &path, body).await.is_ok() {
                            sync_open.set(false);
                            target.set(String::new());
                        }
                    });
                }
            >
                <input
                    required
                    aria-label="Sync target"
                    placeholder="slack:#channel"
                    prop:value=move || target.get()
                    on:input=move |event| target.set(event_target_value(&event))
                />
                <button type="submit" class="button">
                    "Sync"
                </button>
                <button
                    type="button"
                    class="icon-button"
                    aria-label="Cancel sync"
                    on:click=move |_| sync_open.set(false)
                >
                    <Icon name="close" />
                </button>
            </form>
        </Show>
    }
}
