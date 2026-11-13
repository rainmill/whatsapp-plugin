//! The WhatsApp screens: the templates Meta holds, each with a preview.

use std::sync::Arc;

use std::collections::HashMap;

use chrono::{NaiveDate, Utc};
use laterite_admin::axum::extract::{Path, Query};
use laterite_admin::axum::http::StatusCode;
use laterite_admin::axum::response::{IntoResponse, Redirect, Response};
use laterite_admin::axum::routing::{get, post};
use laterite_admin::axum::{Extension, Form, Router};
use laterite_admin::chart::{
    stats, Chart, ChartQuery, Format, Kind, Period, Series, Stat, CHART_ASSETS,
};
use laterite_admin::html::Markup;
use laterite_admin::routes::{RouteCtx, Screen, ScreenReg};
use laterite_admin::ui::Button;
use laterite_admin::{
    AdminRegistry, AuthenticatedUser, FlashLevel, PageHead, Permission, SessionHandle, Shell,
};
use laterite_core::{t, Registry, Text};

use crate::messages::{self, Sent};
use crate::templates::{self, Draft, Template};
use crate::{WhatsAppError, MESSAGES, TEMPLATES};

const SECTION: &str = "rainmill.whatsapp";

pub(crate) fn register(registry: &mut Registry) {
    let group = || t!("WhatsApp");
    registry.add_permission(
        Permission::new(TEMPLATES, t!("Manage message templates"), group())
            .roles([laterite_admin::ROLE_ADMIN]),
    );
    registry.add_permission(
        Permission::new(MESSAGES, t!("Read the message log"), group())
            .roles([laterite_admin::ROLE_ADMIN]),
    );
    registry.add_nav(
        laterite_admin::nav::NavItem::section(SECTION, t!("WhatsApp"))
            .icon("send")
            .description(t!(
                "Message templates, what was sent and what became of it."
            ))
            .slot(laterite_core::arrange::slot::SYSTEM)
            .order(19)
            .in_settings(t!("System")),
    );
    registry.add_screen(
        ScreenReg::new("/templates", TEMPLATES, Arc::new(Templates))
            .in_menu(t!("Templates"))
            .menu(
                laterite_admin::nav::Placement::new()
                    .under(SECTION)
                    .icon("layout-template"),
            ),
    );
    registry.add_screen(
        ScreenReg::new("/messages", MESSAGES, Arc::new(Log))
            .in_menu(t!("Messages"))
            .menu(
                laterite_admin::nav::Placement::new()
                    .under(SECTION)
                    .icon("history"),
            ),
    );
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn failure(e: WhatsAppError) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

/// A template's status as an administrator reads it.
fn status_text(status: &str) -> Text {
    match status {
        "APPROVED" => t!("Approved"),
        "PENDING" => t!("In review"),
        "REJECTED" => t!("Rejected"),
        "PAUSED" => t!("Paused"),
        "DISABLED" => t!("Disabled"),
        "IN_APPEAL" => t!("In appeal"),
        "PENDING_DELETION" => t!("Being deleted"),
        other => Text::dynamic(other.to_string()),
    }
}

/// A category as an administrator reads it.
pub(crate) fn category_text(category: &str) -> Text {
    match category.to_ascii_uppercase().as_str() {
        "MARKETING" => t!("Marketing"),
        "UTILITY" => t!("Utility"),
        "AUTHENTICATION" => t!("Authentication"),
        "SERVICE" => t!("Service"),
        other => Text::dynamic(other.to_string()),
    }
}

fn status_badge(shell: &Shell, status: &str) -> String {
    let tone = match status {
        "APPROVED" => "ok",
        "PENDING" | "IN_APPEAL" => "wait",
        _ => "bad",
    };
    format!(
        r#"<span class="lat-badge" data-tone="{tone}">{}</span>"#,
        escape(&shell.tt(&status_text(status)))
    )
}

struct Templates;

impl Screen for Templates {
    fn mount(&self, ctx: &RouteCtx) -> Router {
        let (index_cx, sync_cx, show_cx) = (ctx.clone(), ctx.clone(), ctx.clone());
        let (new_cx, write_cx, delete_cx) = (ctx.clone(), ctx.clone(), ctx.clone());
        Router::new()
            .route(
                "/new",
                get(move |Extension(shell): Extension<Shell>| {
                    let ctx = new_cx.clone();
                    async move { draft_page(&ctx, shell, &Draft::default(), false, None) }
                })
                .post(
                    move |Extension(shell): Extension<Shell>,
                          Extension(user): Extension<AuthenticatedUser>,
                          Extension(session): Extension<SessionHandle>,
                          Form(data): Form<HashMap<String, String>>| {
                        let ctx = write_cx.clone();
                        async move { write(&ctx, shell, user, session, data).await }
                    },
                ),
            )
            .route(
                "/{id}/delete",
                post(
                    move |Path(id): Path<i64>,
                          Extension(user): Extension<AuthenticatedUser>,
                          Extension(session): Extension<SessionHandle>| {
                        let ctx = delete_cx.clone();
                        async move { delete(&ctx, user, session, id).await }
                    },
                ),
            )
            .route(
                "/",
                get(move |Extension(shell): Extension<Shell>| {
                    let ctx = index_cx.clone();
                    async move { index(&ctx, shell).await }
                }),
            )
            .route(
                "/sync",
                post(
                    move |Extension(user): Extension<AuthenticatedUser>,
                          Extension(session): Extension<SessionHandle>| {
                        let ctx = sync_cx.clone();
                        async move { sync(&ctx, user, session).await }
                    },
                ),
            )
            .route(
                "/{id}",
                get(
                    move |Path(id): Path<i64>, Extension(shell): Extension<Shell>| {
                        let ctx = show_cx.clone();
                        async move { show(&ctx, shell, id).await }
                    },
                ),
            )
    }
}

async fn index(ctx: &RouteCtx, shell: Shell) -> Response {
    let list = match templates::list(ctx.db()).await {
        Ok(list) => list,
        Err(e) => return failure(e),
    };
    let mut body = String::new();
    if list.is_empty() {
        body.push_str(&format!(
            r#"<p class="lat-note">{}</p>"#,
            escape(&shell.t(
                "No templates yet. Store a WhatsApp Cloud API credential under Credentials, attach it to WhatsApp messages, then press Sync now."
            ))
        ));
    } else {
        let mut rows = String::new();
        for template in &list {
            rows.push_str(&format!(
                r#"<tr><td><a href="{href}"><strong>{name}</strong></a></td><td>{language}</td><td>{category}</td><td>{status}{reason}</td><td>{synced}</td></tr>"#,
                href = escape(&ctx.url(&template.id.to_string())),
                name = escape(&template.name),
                language = escape(&template.language),
                category = escape(&shell.tt(&category_text(&template.category))),
                status = status_badge(&shell, &template.status),
                reason = template
                    .rejected_reason
                    .as_ref()
                    .map(|r| format!(
                        r#"<br><span class="lat-muted">{}</span>"#,
                        escape(r)
                    ))
                    .unwrap_or_default(),
                synced = escape(&shell.datetime(&template.synced_at)),
            ));
        }
        body.push_str(&format!(
            r#"<div class="lat-tablewrap"><table class="lat-table"><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table></div>"#,
            escape(&shell.t("Template")),
            escape(&shell.t("Language")),
            escape(&shell.t("Category")),
            escape(&shell.t("Status")),
            escape(&shell.t("Last read from Meta")),
        ));
    }
    let mut actions = Button::link(t!("New template"), ctx.url("new"))
        .primary()
        .render(&shell);
    actions.push(
        Button::post(t!("Sync now"), ctx.url("sync"))
            .icon("refresh-cw")
            .render(&shell),
    );
    shell.page_with(
        PageHead::new(t!("Message templates")).actions(actions),
        Markup::raw_unchecked(body),
    )
}

async fn sync(ctx: &RouteCtx, user: AuthenticatedUser, session: SessionHandle) -> Response {
    match templates::sync(ctx.db(), ctx.crypt()).await {
        Ok(count) => {
            ctx.audit(
                &user,
                "rainmill.whatsapp.templates.sync",
                Some("whatsapp_templates"),
                None,
                None,
                Some(&count.to_string()),
            )
            .await;
            session.push_flash(
                FlashLevel::Success,
                t!("Read {count} templates from Meta.", count = count as i64),
            );
        }
        // Said on the page: a missing credential or a refusal is the
        // administrator's to fix, never an error page.
        Err(e) => session.push_flash(
            FlashLevel::Error,
            Text::dynamic(format!("The templates could not be read: {e}")),
        ),
    }
    Redirect::to(&ctx.url("")).into_response()
}

/// The message as it looks on a phone.
pub(crate) fn bubble(preview: &crate::Preview) -> String {
    let mut inner = String::new();
    if let Some(header) = &preview.header {
        inner.push_str(&format!(
            r#"<div style="font-weight:600;margin-bottom:6px">{}</div>"#,
            escape(header)
        ));
    }
    inner.push_str(&format!(
        r#"<div style="white-space:pre-wrap">{}</div>"#,
        escape(&preview.body)
    ));
    if let Some(footer) = &preview.footer {
        inner.push_str(&format!(
            r#"<div style="margin-top:6px;font-size:12px;opacity:.6">{}</div>"#,
            escape(footer)
        ));
    }
    let mut buttons = String::new();
    for (_, text, target) in &preview.buttons {
        buttons.push_str(&format!(
            r#"<div style="margin-top:4px;padding:9px 12px;border-radius:10px;background:#fff;color:#0a7cff;text-align:center;font-size:14px;box-shadow:0 1px 1px rgba(0,0,0,.12)" title="{}">{}</div>"#,
            escape(target),
            escape(text)
        ));
    }
    format!(
        r#"<div style="max-width:340px;padding:16px;border-radius:14px;background:#e6ddd4"><div style="padding:10px 12px;border-radius:10px;background:#fff;color:#111;font-size:14px;line-height:1.45;box-shadow:0 1px 1px rgba(0,0,0,.12)">{inner}</div>{buttons}</div>"#
    )
}

async fn show(ctx: &RouteCtx, shell: Shell, id: i64) -> Response {
    let template: Template = match templates::find(ctx.db(), id).await {
        Ok(Some(template)) => template,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return failure(e),
    };
    let preview = template.preview(&[], None);
    let fact = |label: &str, value: String| {
        format!(
            r#"<div class="lat-stat"><span>{}</span><b style="font-size:var(--fs-16)">{value}</b></div>"#,
            escape(&shell.t(label))
        )
    };
    let mut links = String::new();
    for (kind, text, target) in &preview.buttons {
        if !target.is_empty() {
            links.push_str(&format!(
                "<li>{} ({}): <code>{}</code></li>",
                escape(text),
                escape(&kind.to_lowercase().replace('_', " ")),
                escape(target)
            ));
        }
    }
    let body = format!(
        r#"<div class="lat-stats">{language}{category}{status}{params}</div><div class="lat-card"><h2 class="lat-section-title">{sample_title}</h2><p class="lat-note">{sample_note}</p>{bubble}{links}</div>{reason}"#,
        language = fact("Language", escape(&template.language)),
        category = fact(
            "Category",
            escape(&shell.tt(&category_text(&template.category)))
        ),
        status = fact("Status", status_badge(&shell, &template.status)),
        params = fact("Values it takes", template.body_params().to_string()),
        sample_title = escape(&shell.t("Sample")),
        sample_note = escape(&shell.t(
            "The message as a recipient reads it, filled with the sample values given to Meta."
        )),
        bubble = bubble(&preview),
        links = if links.is_empty() {
            String::new()
        } else {
            format!(r#"<ul class="lat-note">{links}</ul>"#)
        },
        reason = template
            .rejected_reason
            .as_ref()
            .map(|r| format!(
                r#"<p class="lat-note">{}: {}</p>"#,
                escape(&shell.t("Meta's reason")),
                escape(r)
            ))
            .unwrap_or_default(),
    );
    let remove = Button::post(t!("Delete at Meta"), ctx.url(&format!("{id}/delete")))
        .danger()
        .confirm(t!(
            "Delete this template at Meta? Its name cannot be used again for 30 days."
        ))
        .render(&shell);
    shell.page_with(
        PageHead::new(Text::dynamic(template.name.clone()))
            .back(t!("Message templates"), ctx.url(""))
            .actions(remove)
            .assets(CHART_ASSETS),
        Markup::raw_unchecked(body),
    )
}

/// A message's state as an administrator reads it.
fn state_text(state: &str) -> Text {
    match state {
        "queued" => t!("Waiting to send"),
        "sent" => t!("Sent"),
        "delivered" => t!("Delivered"),
        "read" => t!("Read"),
        "failed" => t!("Failed"),
        other => Text::dynamic(other.to_string()),
    }
}

const STATES: [&str; 5] = ["queued", "sent", "delivered", "read", "failed"];

/// How many messages the log lists and the charts count.
const LOG_LIMIT: u64 = 20_000;
/// How many rows the table shows.
const TABLE_ROWS: usize = 200;

struct Log;

impl Screen for Log {
    fn mount(&self, ctx: &RouteCtx) -> Router {
        let ctx = ctx.clone();
        Router::new().route(
            "/",
            get(
                move |Query(query): Query<ChartQuery>,
                      Query(all): Query<HashMap<String, String>>,
                      Extension(shell): Extension<Shell>| {
                    let ctx = ctx.clone();
                    async move {
                        let state = all
                            .get("state")
                            .filter(|s| STATES.contains(&s.as_str()))
                            .cloned();
                        log(&ctx, shell, query, state).await
                    }
                },
            ),
        )
    }
}

/// The messages of a period: the figures, the charts, then the rows.
async fn log(ctx: &RouteCtx, shell: Shell, query: ChartQuery, state: Option<String>) -> Response {
    let period = Period::from_query(&query, Utc::now().date_naive());
    let all: Vec<Sent> = match messages::log(
        ctx.db(),
        &period.from().to_string(),
        &period.to().to_string(),
        None,
        LOG_LIMIT,
    )
    .await
    {
        Ok(all) => all,
        Err(e) => return failure(e),
    };
    let count = |pick: &dyn Fn(&Sent) -> bool| all.iter().filter(|m| pick(m)).count();
    // A read message was delivered, and a delivered one was sent.
    let reached = |m: &Sent, states: &[&str]| states.contains(&m.state.as_str());
    let accepted = count(&|m| reached(m, &["sent", "delivered", "read"]));
    let delivered = count(&|m| reached(m, &["delivered", "read"]));
    let read = count(&|m| reached(m, &["read"]));
    let failed = count(&|m| reached(m, &["failed"]));
    let rate = |part: usize, whole: usize| match whole {
        0 => "0%".to_string(),
        whole => format!("{:.0}%", part as f64 * 100.0 / whole as f64),
    };
    let number = |n: usize| Format::Number.text(n as f64);
    let mut body = period.bar(&shell, &query).into_string();
    body.push_str(
        stats(
            &shell,
            &[
                Stat::new(t!("Accepted by Meta"), number(accepted)).note(t!("In this period")),
                Stat::new(t!("Delivered"), number(delivered)).note(Text::dynamic(format!(
                    "{} {}",
                    rate(delivered, accepted),
                    shell.t("of accepted")
                ))),
                Stat::new(t!("Read"), number(read)).note(Text::dynamic(format!(
                    "{} {}",
                    rate(read, delivered),
                    shell.t("of delivered")
                ))),
                Stat::new(t!("Failed"), number(failed)).note(t!("In this period")),
                Stat::new(
                    t!("Charged by Meta"),
                    number(count(&|m| m.billable == Some(true))),
                )
                .note(t!("As Meta reported")),
            ],
        )
        .as_str(),
    );

    let day = |m: &Sent| NaiveDate::parse_from_str(m.created_at.get(..10)?, "%Y-%m-%d").ok();
    let dated = |pick: &dyn Fn(&Sent) -> bool| -> Vec<(NaiveDate, f64)> {
        all.iter()
            .filter(|m| pick(m))
            .filter_map(|m| day(m).map(|d| (d, 1.0)))
            .collect()
    };
    let outcome = Chart::new("outcome", period.clone())
        .title(t!("What became of them"))
        .series(Series::daily(
            "accepted",
            t!("Accepted"),
            &period,
            dated(&|m| reached(m, &["sent", "delivered", "read"])),
        ))
        .series(Series::daily(
            "delivered",
            t!("Delivered"),
            &period,
            dated(&|m| reached(m, &["delivered", "read"])),
        ))
        .series(Series::daily(
            "read",
            t!("Read"),
            &period,
            dated(&|m| reached(m, &["read"])),
        ))
        .series(Series::daily(
            "failed",
            t!("Failed"),
            &period,
            dated(&|m| reached(m, &["failed"])),
        ));
    body.push_str(outcome.render(&shell, &query).as_str());

    // One series a category, then one a template, in the order first seen.
    let mut groups =
        |id: &str, title: Text, key: &dyn Fn(&Sent) -> String, label: &dyn Fn(&str) -> Text| {
            let mut names: Vec<String> = Vec::new();
            for m in &all {
                let name = key(m);
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            names.sort();
            if names.is_empty() {
                return;
            }
            let mut chart = Chart::new(id, period.clone())
                .title(title)
                .kind(Kind::StackedBar);
            for name in names.iter().take(12) {
                chart = chart.series(Series::daily(
                    name.clone(),
                    label(name),
                    &period,
                    dated(&|m| key(m) == *name),
                ));
            }
            body.push_str(chart.render(&shell, &query).as_str());
        };
    groups(
        "category",
        t!("By category"),
        &|m| m.category.clone(),
        &|name| category_text(name),
    );
    groups(
        "template",
        t!("By template"),
        &|m| m.template.clone(),
        &|name| Text::dynamic(name.to_string()),
    );

    // The rows, in one state when asked.
    let mut filters = String::new();
    let link = |state: Option<&str>, label: String, on: bool| {
        format!(
            r#"<a class="lat-period__preset{}" href="?{}{}">{}</a>"#,
            if on { " is-on" } else { "" },
            escape(&period.query()),
            state.map(|s| format!("&amp;state={s}")).unwrap_or_default(),
            escape(&label),
        )
    };
    filters.push_str(&link(None, shell.t("All"), state.is_none()));
    for s in STATES {
        filters.push_str(&link(
            Some(s),
            shell.tt(&state_text(s)),
            state.as_deref() == Some(s),
        ));
    }
    let mut rows = String::new();
    for m in all
        .iter()
        .filter(|m| state.as_deref().is_none_or(|s| m.state == s))
        .take(TABLE_ROWS)
    {
        let tone = match m.state.as_str() {
            "failed" => "bad",
            "queued" => "wait",
            _ => "ok",
        };
        rows.push_str(&format!(
            r#"<tr><td>{when}</td><td>{to}</td><td>{template}</td><td>{category}</td><td><span class="lat-badge" data-tone="{tone}">{state}</span>{error}</td></tr>"#,
            when = escape(&shell.datetime(&m.created_at)),
            to = escape(&m.to),
            template = escape(&m.template),
            category = escape(&shell.tt(&category_text(&m.category))),
            state = escape(&shell.tt(&state_text(&m.state))),
            error = m
                .error
                .as_ref()
                .map(|e| format!(r#"<br><span class="lat-muted">{}</span>"#, escape(e)))
                .unwrap_or_default(),
        ));
    }
    let table = if rows.is_empty() {
        format!(
            r#"<p class="lat-note">{}</p>"#,
            escape(&shell.t("No messages in this period."))
        )
    } else {
        format!(
            r#"<div class="lat-tablewrap"><table class="lat-table"><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table></div>"#,
            escape(&shell.t("Queued")),
            escape(&shell.t("To")),
            escape(&shell.t("Template")),
            escape(&shell.t("Category")),
            escape(&shell.t("State")),
        )
    };
    body.push_str(&format!(
        r#"<div class="lat-period"><div class="lat-period__presets">{filters}</div></div>{table}"#
    ));
    shell.page_with(
        PageHead::new(t!("WhatsApp messages")).assets(CHART_ASSETS),
        Markup::raw_unchecked(body),
    )
}

/// Reads the New template form.
fn draft_from(data: &HashMap<String, String>) -> Draft {
    let text = |key: &str| {
        data.get(key)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    Draft {
        name: text("name"),
        category: text("category"),
        language: text("language"),
        header: text("header"),
        body: data
            .get("body")
            .cloned()
            .unwrap_or_default()
            .replace("\r\n", "\n"),
        samples: text("samples")
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        footer: text("footer"),
        button_text: text("button_text"),
        button_url: text("button_url"),
    }
}

/// The New template form. Once a draft has been previewed with nothing wrong,
/// the page shows it as a recipient would read it and offers to submit it.
fn draft_page(
    ctx: &RouteCtx,
    shell: Shell,
    draft: &Draft,
    previewed: bool,
    error: Option<String>,
) -> Response {
    let problems = if previewed {
        draft.problems()
    } else {
        Vec::new()
    };
    let field = |name: &'static str, label: &str, control: String, help: &str| {
        let wrong: String = problems
            .iter()
            .filter(|(field, _)| *field == name)
            .map(|(_, what)| {
                format!(
                    r#"<p class="lat-field__error">{}</p>"#,
                    escape(&shell.t(what))
                )
            })
            .collect();
        let help = if help.is_empty() {
            String::new()
        } else {
            format!(
                r#"<p class="lat-field__help">{}</p>"#,
                escape(&shell.t(help))
            )
        };
        format!(
            r#"<div class="lat-field"><label class="lat-label" for="{name}">{}</label>{control}{help}{wrong}</div>"#,
            escape(&shell.t(label))
        )
    };
    let input = |name: &str, value: &str, extra: &str| {
        format!(
            r#"<input class="lat-input" id="{name}" name="{name}" value="{}" {extra}>"#,
            escape(value)
        )
    };
    let area = |name: &str, value: &str, rows: u8| {
        format!(
            r#"<textarea class="lat-input" id="{name}" name="{name}" rows="{rows}">{}</textarea>"#,
            escape(value)
        )
    };
    let options: String = [
        ("UTILITY", "Utility: about something the person did"),
        ("MARKETING", "Marketing: offers and announcements"),
        ("AUTHENTICATION", "Authentication: one-time codes"),
    ]
    .iter()
    .map(|(value, label)| {
        format!(
            r#"<option value="{value}"{}>{}</option>"#,
            if draft.category.eq_ignore_ascii_case(value) {
                " selected"
            } else {
                ""
            },
            escape(&shell.t(label))
        )
    })
    .collect();
    let ready = previewed && problems.is_empty();
    let mut body = String::new();
    if let Some(error) = error {
        body.push_str(&format!(
            r#"<div class="lat-alert">{}</div>"#,
            escape(&error)
        ));
    }
    body.push_str(&format!(
        r#"<form class="lat-card" method="post" action="{action}"><input type="hidden" name="_csrf" value="{csrf}"><div class="lat-fields">{name}{category}{language}{header}{text}{samples}{footer}{button_text}{button_url}</div><div class="lat-form-actions" style="display:flex;gap:8px"><button class="lat-btn{quiet}" type="submit" name="step" value="preview">{preview}</button>{submit}</div></form>"#,
        action = escape(&ctx.url("new")),
        csrf = escape(shell.csrf_token()),
        name = field(
            "name",
            "Name",
            input("name", &draft.name, r#"maxlength="512" autocapitalize="off" spellcheck="false""#),
            "Lowercase letters, digits and underscores. It cannot be changed, and a deleted name is held for 30 days."
        ),
        category = field(
            "category",
            "Category",
            format!(r#"<select class="lat-input" id="category" name="category">{options}</select>"#),
            "Meta charges by category and may move a template it thinks is in the wrong one."
        ),
        language = field(
            "language",
            "Language",
            input(
                "language",
                if draft.language.is_empty() { "en" } else { &draft.language },
                r#"maxlength="12""#
            ),
            ""
        ),
        header = field("header", "Header", input("header", &draft.header, r#"maxlength="60""#), "Optional. One line above the message."),
        text = field(
            "body",
            "Message",
            area("body", &draft.body, 6),
            "Write {{1}}, {{2}} where a value goes. Anything that varies (a name, a date, a city) is a value, never typed in."
        ),
        samples = field(
            "samples",
            "Sample values",
            area("samples", &draft.samples.join("\n"), 3),
            "One a line, in order: a realistic example of each value. Meta reviews the message with these."
        ),
        footer = field("footer", "Footer", input("footer", &draft.footer, r#"maxlength="60""#), "Optional. One line under the message."),
        button_text = field("button_text", "Button label", input("button_text", &draft.button_text, r#"maxlength="25""#), "Optional. With no address below it is a quick reply."),
        button_url = field("button_url", "Button address", input("button_url", &draft.button_url, r#"inputmode="url""#), "Optional. An https address the button opens."),
        quiet = if ready { "" } else { " lat-btn--primary" },
        preview = escape(&shell.t("Preview")),
        submit = if ready {
            format!(
                r#"<button class="lat-btn lat-btn--primary" type="submit" name="step" value="submit">{}</button>"#,
                escape(&shell.t("Submit to Meta for review"))
            )
        } else {
            String::new()
        },
    ));
    if ready {
        body.push_str(&format!(
            r#"<div class="lat-card"><h2 class="lat-section-title">{}</h2><p class="lat-note">{}</p>{}</div>"#,
            escape(&shell.t("Sample")),
            escape(&shell.t(
                "This is what Meta will review, filled with your sample values. Change anything above and preview again, or submit it."
            )),
            bubble(&draft.as_template().preview(&[], None)),
        ));
    }
    shell.page_with(
        PageHead::new(t!("New template")).back(t!("Message templates"), ctx.url("")),
        Markup::raw_unchecked(body),
    )
}

/// A preview of the form as posted, or its submission to Meta.
async fn write(
    ctx: &RouteCtx,
    shell: Shell,
    user: AuthenticatedUser,
    session: SessionHandle,
    data: HashMap<String, String>,
) -> Response {
    let draft = draft_from(&data);
    let submit = data.get("step").is_some_and(|step| step == "submit");
    if !submit || !draft.problems().is_empty() {
        return draft_page(ctx, shell, &draft, true, None);
    }
    match templates::submit(ctx.db(), ctx.crypt(), &draft).await {
        Ok(()) => {
            ctx.audit(
                &user,
                "rainmill.whatsapp.template.submit",
                Some("whatsapp_templates"),
                None,
                Some(&draft.name),
                Some(&draft.category),
            )
            .await;
            session.push_flash(
                FlashLevel::Success,
                t!("The template was sent to Meta for review."),
            );
            Redirect::to(&ctx.url("")).into_response()
        }
        // Meta's own words, on the form with everything typed kept.
        Err(e) => draft_page(ctx, shell, &draft, true, Some(e.to_string())),
    }
}

async fn delete(
    ctx: &RouteCtx,
    user: AuthenticatedUser,
    session: SessionHandle,
    id: i64,
) -> Response {
    let template = match templates::find(ctx.db(), id).await {
        Ok(Some(template)) => template,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return failure(e),
    };
    match templates::delete(ctx.db(), ctx.crypt(), &template).await {
        Ok(()) => {
            ctx.audit(
                &user,
                "rainmill.whatsapp.template.delete",
                Some("whatsapp_templates"),
                Some(&id.to_string()),
                Some(&template.name),
                Some(&template.language),
            )
            .await;
            session.push_flash(FlashLevel::Success, t!("The template was deleted."));
            Redirect::to(&ctx.url("")).into_response()
        }
        Err(e) => {
            session.push_flash(
                FlashLevel::Error,
                Text::dynamic(format!("The template could not be deleted: {e}")),
            );
            Redirect::to(&ctx.url(&id.to_string())).into_response()
        }
    }
}
