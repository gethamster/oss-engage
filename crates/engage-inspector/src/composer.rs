use leptos::prelude::*;
use serde_json::{Value, json};

use crate::{
    api, content,
    state::{Console, Section},
    ui::{Icon, Markdown},
};

#[component]
pub fn Composer(reply_to: Option<String>) -> impl IntoView {
    let console = expect_context::<Console>();
    let text = RwSignal::new(String::new());
    let channel = RwSignal::new(console.channel.get_untracked());
    let preview = RwSignal::new(false);
    let kind = RwSignal::new(if reply_to.is_some() {
        "message".into()
    } else {
        match console.section.get_untracked() {
            Section::Tasks => "task".to_owned(),
            Section::Schedules => "schedule".to_owned(),
            _ => "message".to_owned(),
        }
    });
    let assigned = RwSignal::new(String::new());
    let needs = RwSignal::new(String::new());
    let schedule_in = RwSignal::new("10m".to_owned());
    let schedule_at = RwSignal::new(String::new());
    let timing = RwSignal::new("in".to_owned());
    let every = RwSignal::new(String::new());
    let times = RwSignal::new(String::new());
    let labels = RwSignal::new(String::new());
    let no_sync = RwSignal::new(false);
    let files = RwSignal::new(Vec::<Value>::new());
    let uploading = RwSignal::new(false);
    let request_id = RwSignal::new(new_request_id());
    let is_reply = reply_to.is_some();
    let target = StoredValue::new(reply_to);
    let form_id = if is_reply { "reply" } else { "compose" };
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let draft = text.get_untracked();
        let attachment_ids: Vec<Value> = files
            .get_untracked()
            .iter()
            .filter_map(|file| file.get("id").cloned())
            .collect();
        if draft.trim().is_empty() && attachment_ids.is_empty() {
            return;
        }
        let mut body =
            json!({"text":draft,"attachments":attachment_ids,"id":request_id.get_untracked()});
        if let Some(re) = target.get_value() {
            body["re"] = re.into();
            let thread = console.thread.get_untracked();
            let newest = thread
                .get("replies")
                .and_then(Value::as_array)
                .and_then(|replies| replies.last())
                .or_else(|| thread.get("message"));
            if let Some(id) = newest.and_then(|message| message.get("id")) {
                body["after"] = id.clone();
            }
        } else {
            body["channel"] = channel.get_untracked().into();
        }
        let mut parsed_labels = serde_json::Map::new();
        for part in labels
            .get_untracked()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            parsed_labels.insert(key.trim().into(), value.trim().into());
        }
        if !parsed_labels.is_empty() {
            body["labels"] = parsed_labels.into();
        }
        if no_sync.get_untracked() {
            body["noSync"] = true.into();
        }
        let mode = kind.get_untracked();
        let endpoint = match mode.as_str() {
            "batch" if !is_reply => {
                let tasks = match serde_json::from_str::<Value>(&draft) {
                    Ok(Value::Array(tasks)) if !tasks.is_empty() => tasks,
                    _ => {
                        console
                            .error
                            .set("Enter a JSON array containing at least one task.".into());
                        return;
                    }
                };
                body = json!({"channel":channel.get_untracked(),"tasks":tasks,"id":request_id.get_untracked()});
                "/tasks/batch"
            }
            "task" if !is_reply => {
                if !assigned.get_untracked().trim().is_empty() {
                    body["for"] = assigned.get_untracked().trim().into();
                }
                body["needs"] = json!(
                    needs
                        .get_untracked()
                        .split([',', ' '])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                );
                "/tasks"
            }
            "schedule" => {
                if let Some(map) = body.as_object_mut() {
                    map.remove("after");
                    map.remove("id");
                }
                if timing.get_untracked() == "at" {
                    let date = js_sys::Date::new(&schedule_at.get_untracked().into());
                    if date.get_time().is_nan() {
                        console.error.set("Choose a valid send time.".into());
                        return;
                    }
                    body["at"] = date.to_iso_string().as_string().unwrap_or_default().into();
                } else if !schedule_in.get_untracked().trim().is_empty() {
                    body["in"] = schedule_in.get_untracked().trim().into();
                }
                if !every.get_untracked().trim().is_empty() {
                    body["every"] = every.get_untracked().trim().into();
                }
                if let Ok(count) = times.get_untracked().parse::<u64>() {
                    body["times"] = count.into();
                }
                "/schedules"
            }
            _ => "/messages",
        };
        leptos::task::spawn_local(async move {
            if let Ok(value) = console.mutate("POST", endpoint, body).await {
                text.set(String::new());
                request_id.set(new_request_id());
                files.set(Vec::new());
                if !is_reply && mode != "schedule" {
                    let message = value
                        .get("message")
                        .or_else(|| value.get("task"))
                        .or_else(|| {
                            value
                                .get("tasks")
                                .and_then(Value::as_array)
                                .and_then(|tasks| tasks.first())
                        })
                        .unwrap_or(&value);
                    let id = content::field(message, "id");
                    console.composing.set(false);
                    if !id.is_empty() {
                        console.open_thread(id.into());
                    }
                }
                if mode == "schedule" {
                    console.composing.set(false);
                }
            }
        });
    };
    view! {
        <form class="composer" on:submit=submit>
            <Show when=move || !is_reply>
                <div class="form-row">
                    <label for=format!("{form_id}-channel")>"Channel"</label>
                    <input
                        id=format!("{form_id}-channel")
                        required
                        prop:value=move || channel.get()
                        on:input=move |event| channel.set(event_target_value(&event))
                    />
                </div>
            </Show>
            <div class="composer-tabs">
                <button
                    type="button"
                    class:active=move || !preview.get()
                    on:click=move |_| preview.set(false)
                >
                    "Write"
                </button>
                <button
                    type="button"
                    class:active=move || preview.get()
                    on:click=move |_| preview.set(true)
                >
                    "Preview"
                </button>
                <select
                    aria-label="Message kind"
                    prop:value=move || kind.get()
                    on:change=move |event| kind.set(event_target_value(&event))
                >
                    <option value="message">{if is_reply { "Reply" } else { "Message" }}</option>
                    <Show when=move || !is_reply>
                        <option value="task">"Task"</option>
                        <option value="batch">"Task batch"</option>
                    </Show>
                    <option value="schedule">"Schedule"</option>
                </select>
            </div>
            <Show
                when=move || !preview.get()
                fallback=move || {
                    view! {
                        <div class="composer-preview">
                            <Markdown text=text.get() />
                        </div>
                    }
                }
            >
                <label class="visually-hidden" for=format!("{form_id}-text")>
                    {move || {
                        if kind.get() == "batch" {
                            "Tasks JSON"
                        } else if is_reply {
                            "Reply"
                        } else {
                            "Message"
                        }
                    }}
                </label>
                <textarea
                    id=format!("{form_id}-text")
                    class="compose-text"
                    rows=if is_reply { 4 } else { 9 }
                    placeholder=move || {
                        if kind.get() == "batch" {
                            "[{\"text\":\"Task description\",\"key\":\"build\"}]"
                        } else if is_reply {
                            "Write a reply…"
                        } else {
                            "What are we working on?"
                        }
                    }
                    prop:value=move || text.get()
                    on:input=move |event| text.set(event_target_value(&event))
                />
            </Show>
            <Show when=move || !files.get().is_empty()>
                <div class="attachment-drafts">
                    <For
                        each=move || files.get()
                        key=|file| content::field(file, "id").to_owned()
                        children=move |file| {
                            let id = content::field(&file, "id").to_owned();
                            let name = content::field(&file, "name").to_owned();
                            view! {
                                <span class="attachment-draft">
                                    <Icon name="attach" />
                                    {name}
                                    <button
                                        type="button"
                                        class="icon-button"
                                        aria-label="Remove attachment"
                                        on:click=move |_| {
                                            files
                                                .update(|items| {
                                                    items.retain(|item| content::field(item, "id") != id)
                                                })
                                        }
                                    >
                                        <Icon name="close" />
                                    </button>
                                </span>
                            }
                        }
                    />
                </div>
            </Show>
            <Show when=move || kind.get() == "task" && !is_reply>
                <div class="form-row">
                    <label for=format!("{form_id}-assign")>"Assign to"</label>
                    <input
                        id=format!("{form_id}-assign")
                        placeholder="Actor ID (optional)"
                        prop:value=move || assigned.get()
                        on:input=move |event| assigned.set(event_target_value(&event))
                    />
                </div>
                <div class="form-row">
                    <label for=format!("{form_id}-needs")>"Dependencies"</label>
                    <input
                        id=format!("{form_id}-needs")
                        placeholder="Task IDs, separated by commas"
                        prop:value=move || needs.get()
                        on:input=move |event| needs.set(event_target_value(&event))
                    />
                </div>
            </Show>
            <Show when=move || kind.get() == "schedule">
                <label class="visually-hidden" for=format!("{form_id}-timing")>
                    "Schedule timing"
                </label>
                <select
                    id=format!("{form_id}-timing")
                    prop:value=move || timing.get()
                    on:change=move |event| timing.set(event_target_value(&event))
                >
                    <option value="in">"Send after a delay"</option>
                    <option value="at">"Send at a time"</option>
                </select>
                <div class="form-grid">
                    <Show
                        when=move || timing.get() == "in"
                        fallback=move || {
                            view! {
                                <div class="form-row">
                                    <label for=format!("{form_id}-at")>"Send at"</label>
                                    <input
                                        id=format!("{form_id}-at")
                                        type="datetime-local"
                                        required
                                        prop:value=move || schedule_at.get()
                                        on:input=move |event| {
                                            schedule_at.set(event_target_value(&event))
                                        }
                                    />
                                </div>
                            }
                        }
                    >
                        <div class="form-row">
                            <label for=format!("{form_id}-in")>"Send in"</label>
                            <input
                                id=format!("{form_id}-in")
                                placeholder="10m"
                                prop:value=move || schedule_in.get()
                                on:input=move |event| schedule_in.set(event_target_value(&event))
                            />
                        </div>
                    </Show>
                    <div class="form-row">
                        <label for=format!("{form_id}-every")>"Repeat every"</label>
                        <input
                            id=format!("{form_id}-every")
                            placeholder="1d (optional)"
                            prop:value=move || every.get()
                            on:input=move |event| every.set(event_target_value(&event))
                        />
                    </div>
                    <div class="form-row">
                        <label for=format!("{form_id}-times")>"Sends"</label>
                        <input
                            id=format!("{form_id}-times")
                            type="number"
                            min="1"
                            placeholder="No limit"
                            prop:value=move || times.get()
                            on:input=move |event| times.set(event_target_value(&event))
                        />
                    </div>
                </div>
            </Show>
            <details class="composer-options">
                <summary>"Options"</summary>
                <div class="form-row">
                    <label for=format!("{form_id}-labels")>"Labels"</label>
                    <input
                        id=format!("{form_id}-labels")
                        placeholder="priority=high, team=platform"
                        prop:value=move || labels.get()
                        on:input=move |event| labels.set(event_target_value(&event))
                    />
                </div>
                <label class="checkbox">
                    <input
                        type="checkbox"
                        prop:checked=move || no_sync.get()
                        on:change=move |event| no_sync.set(event_target_checked(&event))
                    />
                    "Keep on this board"
                </label>
            </details>
            <div class="composer-actions">
                <label class="button attach-control">
                    <Icon name="attach" />
                    <span>{move || if uploading.get() { "Uploading…" } else { "Attach" }}</span>
                    <input
                        type="file"
                        multiple
                        aria-label="Attach files"
                        disabled=move || uploading.get()
                        on:change=move |event| {
                            let input = event_target::<web_sys::HtmlInputElement>(&event);
                            let Some(selected) = input.files() else { return };
                            let selected: Vec<_> = (0..selected.length())
                                .filter_map(|index| selected.get(index))
                                .collect();
                            input.set_value("");
                            uploading.set(true);
                            leptos::task::spawn_local(async move {
                                for file in selected {
                                    match api::upload(file).await {
                                        Ok(value) => {
                                            let value = value
                                                .get("attachment")
                                                .unwrap_or(&value)
                                                .clone();
                                            files.update(|files| files.push(value));
                                        }
                                        Err(error) => console.report(error),
                                    }
                                }
                                uploading.set(false);
                            });
                        }
                    />
                </label>
                <button
                    type="submit"
                    class="button primary"
                    disabled=move || {
                        console.busy.get() || uploading.get()
                            || (text.get().trim().is_empty() && files.get().is_empty())
                    }
                >
                    <Icon name="send" />
                    {move || {
                        if console.busy.get() {
                            "Sending…"
                        } else if kind.get() == "batch" {
                            "Create batch"
                        } else if kind.get() == "task" {
                            "Create task"
                        } else if kind.get() == "schedule" {
                            "Schedule"
                        } else if is_reply {
                            "Reply"
                        } else {
                            "Post"
                        }
                    }}
                </button>
            </div>
        </form>
    }
}

fn new_request_id() -> String {
    format!(
        "inspector-{}-{:016x}",
        js_sys::Date::now() as u64,
        (js_sys::Math::random() * u64::MAX as f64) as u64
    )
}
