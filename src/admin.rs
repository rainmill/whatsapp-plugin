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
use crate::{view, WhatsAppError, MESSAGES, TEMPLATES};

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
    registry.add_asset(view::CSS, view::css());
    registry.add_asset(view::JS, view::js());
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

pub(crate) use crate::view::{category_text, escape};

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

fn status_tone(status: &str) -> &'static str {
    match status {
        "APPROVED" => "ok",
        "PENDING" | "IN_APPEAL" | "DRAFT" => "wait",
        _ => "bad",
    }
}

fn status_badge(shell: &Shell, status: &str) -> String {
    view::pill(status_tone(status), &shell.tt(&status_text(status)))
}

/// How far back the Templates screens count messages, in days.
const COUNT_DAYS: i64 = 30;

fn count_from() -> String {
    (Utc::now() - chrono::Duration::days(COUNT_DAYS))
        .format("%Y-%m-%d")
        .to_string()
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
                get(
                    move |Extension(shell): Extension<Shell>,
                          Query(query): Query<HashMap<String, String>>| {
                        let ctx = index_cx.clone();
                        async move { index(&ctx, shell, query).await }
                    },
                ),
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
                    move |Path(id): Path<i64>,
                          Extension(shell): Extension<Shell>,
                          Query(query): Query<HashMap<String, String>>| {
                        let ctx = show_cx.clone();
                        let written = query.get("as").is_some_and(|v| v == "written");
                        async move { show(&ctx, shell, id, written).await }
                    },
                ),
            )
    }
}

async fn index(ctx: &RouteCtx, shell: Shell, query: HashMap<String, String>) -> Response {
    let all = match templates::list(ctx.db()).await {
        Ok(list) => list,
        Err(e) => return failure(e),
    };
    let tallies = messages::tallies(ctx.db(), &count_from())
        .await
        .unwrap_or_default();
    let asked = |key: &str| query.get(key).map(|v| v.trim()).unwrap_or_default();
    let (q, category, status) = (
        asked("q").to_lowercase(),
        asked("category").to_ascii_uppercase(),
        asked("status").to_ascii_uppercase(),
    );
    let list: Vec<&Template> = all
        .iter()
        .filter(|t| {
            (q.is_empty()
                || t.name.to_lowercase().contains(&q)
                || t.body_text().to_lowercase().contains(&q))
                && (category.is_empty() || t.category.eq_ignore_ascii_case(&category))
                && (status.is_empty() || t.status.eq_ignore_ascii_case(&status))
        })
        .collect();
    let mut body = String::new();
    if all.is_empty() {
        body.push_str(&format!(
            r#"<p class="lat-note">{}</p>"#,
            escape(&shell.t(
                "No templates yet. Store a WhatsApp Cloud API credential under Credentials, attach it to WhatsApp messages, then press Sync now."
            ))
        ));
    } else {
        let options = |chosen: &str, all_label: &str, values: &[(&str, Text)]| {
            let mut out = format!(
                r#"<option value="">{}</option>"#,
                escape(&shell.t(all_label))
            );
            for (value, label) in values {
                out.push_str(&format!(
                    r#"<option value="{value}"{}>{}</option>"#,
                    if chosen == *value { " selected" } else { "" },
                    escape(&shell.tt(label))
                ));
            }
            out
        };
        body.push_str(&format!(
            r#"<form class="wa-filters" method="get" role="search"><input class="lat-input" type="search" name="q" value="{q}" placeholder="{search}" aria-label="{search}"><select class="lat-input" name="category" aria-label="{category_label}">{categories}</select><select class="lat-input" name="status" aria-label="{status_label}">{statuses}</select><button class="lat-btn lat-btn--secondary" type="submit">{apply}</button></form>"#,
            q = escape(asked("q")),
            search = escape(&shell.t("Search by name or wording")),
            category_label = escape(&shell.t("Category")),
            categories = options(
                &category,
                "Every category",
                &["MARKETING", "UTILITY", "AUTHENTICATION"].map(|c| (c, category_text(c))),
            ),
            status_label = escape(&shell.t("Status")),
            statuses = options(
                &status,
                "Every status",
                &["APPROVED", "PENDING", "REJECTED", "PAUSED", "DISABLED"]
                    .map(|s| (s, status_text(s))),
            ),
            apply = escape(&shell.t("Filter")),
        ));
        let mut rows = String::new();
        for template in &list {
            let tally = tallies
                .get(&(template.name.clone(), template.language.clone()))
                .copied()
                .unwrap_or_default();
            let number = |n: i64| {
                if n == 0 {
                    r#"<span class="lat-muted">0</span>"#.to_string()
                } else {
                    n.to_string()
                }
            };
            rows.push_str(&format!(
                r#"<tr><td><div class="wa-name">{tile}<div><a href="{href}">{name}</a><span class="wa-snippet">{snippet}</span></div></div></td><td>{category}<span class="wa-sub">{language}</span></td><td>{status}{reason}</td><td class="wa-num">{sent}</td><td class="wa-num">{delivered}</td><td class="wa-num">{read}</td><td class="wa-num">{failed}</td><td class="wa-num">{charged}</td></tr>"#,
                tile = view::tile(&shell, &template.category, true),
                href = escape(&ctx.url(&template.id.to_string())),
                name = escape(&template.name),
                snippet = escape(&template.body_text().replace('\n', " ")),
                category = escape(&shell.tt(&category_text(&template.category))),
                language = escape(&template.language),
                status = status_badge(&shell, &template.status),
                reason = template
                    .rejected_reason
                    .as_ref()
                    .map(|r| format!(r#"<span class="wa-sub">{}</span>"#, escape(r)))
                    .unwrap_or_default(),
                sent = number(tally.sent),
                delivered = number(tally.delivered),
                read = number(tally.read),
                failed = number(tally.failed),
                charged = number(tally.charged),
            ));
        }
        if rows.is_empty() {
            body.push_str(&format!(
                r#"<p class="lat-note">{}</p>"#,
                escape(&shell.t("No template matches."))
            ));
        } else {
            let head = |label: &str, numeric: bool| {
                format!(
                    r#"<th{}>{}</th>"#,
                    if numeric { r#" class="wa-num""# } else { "" },
                    escape(&shell.t(label))
                )
            };
            body.push_str(&format!(
                r#"<div class="lat-tablewrap"><table class="lat-table wa-list"><thead><tr>{}{}{}{}{}{}{}{}</tr></thead><tbody>{rows}</tbody></table></div><p class="lat-note">{}</p>"#,
                head("Template", false),
                head("Category", false),
                head("Status", false),
                head("Sent", true),
                head("Delivered", true),
                head("Read", true),
                head("Failed", true),
                head("Charged", true),
                escape(&shell.tf(
                    "The counts are of messages queued in the last {n} days.",
                    &[("n", COUNT_DAYS)]
                )),
            ));
        }
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
        PageHead::new(t!("Message templates"))
            .actions(actions)
            .assets(&[view::CSS]),
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

async fn show(ctx: &RouteCtx, shell: Shell, id: i64, written: bool) -> Response {
    let template: Template = match templates::find(ctx.db(), id).await {
        Ok(Some(template)) => template,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return failure(e),
    };
    let preview = if written {
        template.outline()
    } else {
        template.preview(&[], None)
    };
    let tally = messages::tallies(ctx.db(), &count_from())
        .await
        .unwrap_or_default()
        .get(&(template.name.clone(), template.language.clone()))
        .copied()
        .unwrap_or_default();
    let recent = messages::recent(ctx.db(), &template.name, &template.language, 10)
        .await
        .unwrap_or_default();
    let sep = r#"<span class="wa-meta__sep">·</span>"#;
    let head = format!(
        r#"<div class="wa-head">{tile}<div><h2 class="wa-head__name">{name} <span class="lat-muted">· {language}</span></h2><div class="wa-meta">{status}{sep}<span title="{cost}">{category}</span>{sep}<span>{read_label} {synced}</span>{meta_id}</div></div></div>"#,
        tile = view::tile(&shell, &template.category, false),
        name = escape(&template.name),
        language = escape(&template.language),
        status = status_badge(&shell, &template.status),
        cost = escape(&shell.tt(&view::category_cost(&template.category))),
        category = escape(&shell.tt(&category_text(&template.category))),
        read_label = escape(&shell.t("Read from Meta")),
        synced = escape(&shell.datetime(&template.synced_at)),
        meta_id = if template.wa_id.is_empty() {
            String::new()
        } else {
            format!(
                r#"{sep}<span>{} <code>{}</code></span>"#,
                escape(&shell.t("Meta ID")),
                escape(&template.wa_id)
            )
        },
    );
    let reason = template
        .rejected_reason
        .as_ref()
        .map(|r| {
            format!(
                r#"<div class="lat-alert"><strong>{}</strong> {}</div>"#,
                escape(&shell.t("Meta's reason:")),
                escape(r)
            )
        })
        .unwrap_or_default();
    let here = ctx.url(&id.to_string());
    let side = format!(
        r#"<div class="lat-card"><div class="wa-preview__head"><h2 class="lat-section-title">{title}</h2><span class="wa-toggle"><a href="{here}"{on_sample}>{sample}</a><a href="{here}?as=written"{on_written}>{as_written}</a></span></div>{phone}</div>"#,
        title = escape(&shell.t("Your template")),
        here = escape(&here),
        on_sample = if written { "" } else { r#" class="is-on""# },
        on_written = if written { r#" class="is-on""# } else { "" },
        sample = escape(&shell.t("With samples")),
        as_written = escape(&shell.t("As written")),
        phone = view::phone(&shell, &preview),
    );
    let stat = |label: &str, value: i64| {
        format!(
            r#"<div class="lat-stat"><span>{}</span><b style="font-size:var(--fs-20)">{value}</b></div>"#,
            escape(&shell.t(label))
        )
    };
    let mut main = format!(
        r#"<div class="lat-card"><h2 class="lat-section-title">{title}</h2><p class="lat-note">{note}</p><div class="lat-stats wa-stats">{sent}{delivered}{read}{failed}{charged}</div><p class="lat-note" style="margin:0"><strong>{category}:</strong> {cost}.</p></div>"#,
        title = escape(&shell.t("How it has done")),
        note = escape(&shell.tf(
            "Messages queued with this template in the last {n} days.",
            &[("n", COUNT_DAYS)]
        )),
        sent = stat("Sent", tally.sent),
        delivered = stat("Delivered", tally.delivered),
        read = stat("Read", tally.read),
        failed = stat("Failed", tally.failed),
        charged = stat("Charged", tally.charged),
        category = escape(&shell.tt(&category_text(&template.category))),
        cost = escape(&shell.tt(&view::category_cost(&template.category))),
    );
    let samples = template.body_samples();
    let takes = template.body_params();
    if takes > 0 {
        let mut rows = String::new();
        for n in 1..=takes {
            rows.push_str(&format!(
                r#"<tr><td><span class="wa-var">{{{{{n}}}}}</span></td><td>{}</td></tr>"#,
                samples
                    .get(n - 1)
                    .map(|s| escape(s))
                    .unwrap_or_else(|| r#"<span class="lat-muted">-</span>"#.to_string())
            ));
        }
        main.push_str(&format!(
            r#"<div class="lat-card"><h2 class="lat-section-title">{}</h2><p class="lat-note">{}</p><div class="lat-tablewrap"><table class="lat-table wa-vars"><thead><tr><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table></div></div>"#,
            escape(&shell.t("Values it takes")),
            escape(&shell.t(
                "Each is filled in when a message is sent. Meta reviewed the template with the sample beside it."
            )),
            escape(&shell.t("Value")),
            escape(&shell.t("Sample")),
        ));
    }
    let buttons: Vec<_> = template.outline().buttons;
    if !buttons.is_empty() {
        let mut rows = String::new();
        for (kind, text, target) in &buttons {
            let kind = match kind.as_str() {
                "URL" => shell.t("Opens an address"),
                "PHONE_NUMBER" => shell.t("Calls a number"),
                "COPY_CODE" => shell.t("Copies a code"),
                _ => shell.t("Quick reply"),
            };
            rows.push_str(&format!(
                r#"<tr><td>{}</td><td>{}</td><td>{}</td></tr>"#,
                escape(text),
                escape(&kind),
                if target.is_empty() {
                    String::new()
                } else {
                    format!(
                        r#"<code style="overflow-wrap:anywhere">{}</code>"#,
                        escape(target)
                    )
                }
            ));
        }
        main.push_str(&format!(
            r#"<div class="lat-card"><h2 class="lat-section-title">{}</h2><div class="lat-tablewrap"><table class="lat-table"><thead><tr><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table></div></div>"#,
            escape(&shell.t("Buttons")),
            escape(&shell.t("Label")),
            escape(&shell.t("What it does")),
            escape(&shell.t("Where it goes")),
        ));
    }
    if !recent.is_empty() {
        let mut rows = String::new();
        for m in &recent {
            rows.push_str(&format!(
                r#"<tr><td>{}</td><td>{}</td><td>{}{}</td></tr>"#,
                escape(&shell.datetime(&m.created_at)),
                escape(&m.to),
                state_badge(&shell, &m.state),
                m.error
                    .as_ref()
                    .map(|e| format!(r#"<span class="wa-error">{}</span>"#, escape(e)))
                    .unwrap_or_default(),
            ));
        }
        main.push_str(&format!(
            r#"<div class="lat-card"><h2 class="lat-section-title">{}</h2><div class="lat-tablewrap"><table class="lat-table"><thead><tr><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table></div></div>"#,
            escape(&shell.t("Latest messages")),
            escape(&shell.t("Queued")),
            escape(&shell.t("To")),
            escape(&shell.t("State")),
        ));
    }
    let body = format!(
        r#"{head}{reason}<div class="wa-split"><div class="wa-split__main">{main}</div><div class="wa-split__side">{side}</div></div>"#
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
            .assets(CHART_ASSETS)
            .assets(&[view::CSS]),
        Markup::raw_unchecked(body),
    )
}

fn state_tone(state: &str) -> &'static str {
    match state {
        "failed" => "bad",
        "queued" => "wait",
        _ => "ok",
    }
}

fn state_badge(shell: &Shell, state: &str) -> String {
    view::pill(state_tone(state), &shell.tt(&state_text(state)))
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
        rows.push_str(&format!(
            r#"<tr><td>{when}</td><td>{to}</td><td>{template}</td><td>{category}</td><td>{state}{error}</td></tr>"#,
            when = escape(&shell.datetime(&m.created_at)),
            to = escape(&m.to),
            template = escape(&m.template),
            category = escape(&shell.tt(&category_text(&m.category))),
            state = state_badge(&shell, &m.state),
            error = m
                .error
                .as_ref()
                .map(|e| format!(r#"<span class="wa-error">{}</span>"#, escape(e)))
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
        PageHead::new(t!("WhatsApp messages"))
            .assets(CHART_ASSETS)
            .assets(&[view::CSS]),
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
    // One box a value, in order; a form posted without the script's boxes
    // gives them one a line.
    let mut samples: Vec<String> = (1..=99)
        .map_while(|n| data.get(&format!("sample_{n}")))
        .map(|v| v.trim().to_string())
        .collect();
    if samples.is_empty() {
        samples = text("samples")
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
    }
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
        samples,
        footer: text("footer"),
        button_text: text("button_text"),
        button_url: text("button_url"),
    }
}

/// The New template form beside the message as a phone shows it. Once a
/// draft has been checked with nothing wrong, the page offers to submit it.
fn draft_page(
    ctx: &RouteCtx,
    shell: Shell,
    draft: &Draft,
    checked: bool,
    error: Option<String>,
) -> Response {
    let problems = if checked {
        draft.problems()
    } else {
        Vec::new()
    };
    let wrong = |name: &str| -> String {
        problems
            .iter()
            .filter(|(field, _)| *field == name)
            .map(|(_, what)| {
                format!(
                    r#"<p class="lat-field__error">{}</p>"#,
                    escape(&shell.t(what))
                )
            })
            .collect()
    };
    // A field: its label, a count when it has a limit, the control, the help
    // and what is wrong with it.
    let field = |name: &'static str,
                 label: &str,
                 limit: Option<u16>,
                 control: String,
                 help: &str| {
        let count = limit
            .map(|max| {
                format!(r#"<span class="wa-count" data-wa-count="{name}" data-max="{max}"></span>"#)
            })
            .unwrap_or_default();
        let help = if help.is_empty() {
            String::new()
        } else {
            format!(
                r#"<p class="lat-field__help">{}</p>"#,
                escape(&shell.t(help))
            )
        };
        format!(
            r#"<div class="lat-field">{count}<label class="lat-label" for="{name}">{}</label>{control}{help}{}</div>"#,
            escape(&shell.t(label)),
            wrong(name)
        )
    };
    let input = |name: &str, value: &str, extra: &str| {
        format!(
            r#"<input class="lat-input" id="{name}" name="{name}" value="{}" {extra}>"#,
            escape(value)
        )
    };
    let options: String = ["UTILITY", "MARKETING", "AUTHENTICATION"]
        .iter()
        .map(|value| {
            format!(
                r#"<option value="{value}"{}>{}: {}</option>"#,
                if draft.category.eq_ignore_ascii_case(value) {
                    " selected"
                } else {
                    ""
                },
                escape(&shell.tt(&category_text(value))),
                escape(&shell.tt(&view::category_cost(value)).to_lowercase()),
            )
        })
        .collect();
    let section = |title: &str, note: &str, inner: String| {
        format!(
            r#"<div class="lat-card"><h2 class="lat-section-title">{}</h2>{}{inner}</div>"#,
            escape(&shell.t(title)),
            if note.is_empty() {
                String::new()
            } else {
                format!(r#"<p class="lat-note">{}</p>"#, escape(&shell.t(note)))
            }
        )
    };
    let takes = draft.as_template().body_params().max(draft.samples.len());
    let mut sample_rows = String::new();
    for n in 1..=takes {
        sample_rows.push_str(&format!(
            r#"<div class="wa-sample" data-wa-sample><code>{{{{{n}}}}}</code><input class="lat-input" name="sample_{n}" value="{}" maxlength="200" autocomplete="off" aria-label="{}"></div>"#,
            escape(draft.samples.get(n - 1).map(String::as_str).unwrap_or_default()),
            escape(&shell.tfs("A sample of {n}", &[("n", &format!("{{{{{n}}}}}"))])),
        ));
    }
    let samples = format!(
        r#"<div class="lat-field"><span class="lat-label">{label}</span><div class="wa-samples" data-wa-samples data-label="{aria}"><p class="wa-samples__none" data-wa-none{hidden}>{none}</p>{sample_rows}</div><p class="lat-field__help">{help}</p>{wrong}</div>"#,
        label = escape(&shell.t("Sample values")),
        aria = escape(&shell.t("A sample of {n}")),
        hidden = if takes > 0 { " hidden" } else { "" },
        none = escape(&shell.t("The message takes no values yet. Write {{1}} where one goes, or press Add a value.")),
        help = escape(&shell.t(
            "A realistic example of each value. Meta reviews the message with these; they are never sent. Use no real person's details."
        )),
        wrong = wrong("samples"),
    );
    let naming = section(
        "Name, language and category",
        "",
        format!(
            r#"<div class="wa-pair">{}{}</div>{}"#,
            field(
                "name",
                "Name",
                Some(512),
                input("name", &draft.name, r#"maxlength="512" autocapitalize="off" spellcheck="false""#),
                "Lowercase letters, digits and underscores. It cannot be changed, and a deleted name is held for 30 days."
            ),
            field(
                "language",
                "Language",
                None,
                input(
                    "language",
                    if draft.language.is_empty() { "en" } else { &draft.language },
                    r#"maxlength="12""#
                ),
                "A code such as en or en_US."
            ),
            field(
                "category",
                "Category",
                None,
                format!(r#"<select class="lat-input" id="category" name="category">{options}</select>"#),
                "The category sets what Meta charges, and Meta may move a template it thinks is in the wrong one."
            ),
        ),
    );
    let content = section(
        "Content",
        "A header, the message and a footer. Meta reviews the wording and the values before the template can be sent.",
        format!(
            "{}{}{samples}{}",
            field(
                "header",
                "Header",
                Some(60),
                input("header", &draft.header, r#"maxlength="60""#),
                "Optional. One line above the message, in bold."
            ),
            field(
                "body",
                "Message",
                Some(1024),
                format!(
                    r#"<textarea class="lat-input" id="body" name="body" rows="7">{}</textarea><div class="wa-tools"><button type="button" class="lat-btn lat-btn--secondary lat-btn--sm" data-wa-add-value>{}</button><span class="wa-tools__hint">{}</span></div>"#,
                    escape(&draft.body),
                    escape(&shell.t("Add a value")),
                    escape(&shell.t("*bold*  _italic_  ~struck~")),
                ),
                "Write {{1}}, {{2}} where a value goes. Anything that varies (a name, a date, a city) is a value, never typed in."
            ),
            field(
                "footer",
                "Footer",
                Some(60),
                input("footer", &draft.footer, r#"maxlength="60""#),
                "Optional. One small line under the message."
            ),
        ),
    );
    let button = section(
        "Button",
        "Optional. One button under the message.",
        format!(
            r#"<div class="wa-pair">{}{}</div>"#,
            field(
                "button_url",
                "Address it opens",
                None,
                input(
                    "button_url",
                    &draft.button_url,
                    r#"inputmode="url" placeholder="https://""#
                ),
                "An https address. With none, the button is a quick reply."
            ),
            field(
                "button_text",
                "Label",
                Some(25),
                input("button_text", &draft.button_text, r#"maxlength="25""#),
                ""
            ),
        ),
    );
    let ready = checked && problems.is_empty();
    let mut main = String::new();
    if let Some(error) = error {
        main.push_str(&format!(
            r#"<div class="lat-alert">{}</div>"#,
            escape(&error)
        ));
    }
    if ready {
        main.push_str(&format!(
            r#"<p class="lat-note">{}</p>"#,
            escape(&shell.t(
                "Nothing is wrong with it. The preview shows what Meta will review, filled with your samples. Change anything and check again, or submit it."
            ))
        ));
    }
    main.push_str(&format!(
        r#"{naming}{content}{button}<div class="lat-form-actions" style="display:flex;gap:8px"><button class="lat-btn{quiet}" type="submit" name="step" value="preview">{check}</button>{submit}</div>"#,
        quiet = if ready { " lat-btn--secondary" } else { " lat-btn--primary" },
        check = escape(&shell.t("Check")),
        submit = if ready {
            format!(
                r#"<button class="lat-btn lat-btn--primary" type="submit" name="step" value="submit">{}</button>"#,
                escape(&shell.t("Submit to Meta for review"))
            )
        } else {
            String::new()
        },
    ));
    let side = format!(
        r#"<div class="lat-card"><div class="wa-preview__head"><h2 class="lat-section-title">{}</h2></div><div class="wa-chat" data-wa-preview data-empty="{}" data-time="{}">{}</div></div>"#,
        escape(&shell.t("Preview")),
        escape(&shell.t("Your message appears here.")),
        view::TIME,
        view::bubble(&shell, &draft.as_template().preview(&[], None)),
    );
    let body = format!(
        r#"<form method="post" action="{action}" data-wa-editor><input type="hidden" name="_csrf" value="{csrf}"><div class="wa-split"><div class="wa-split__main">{main}</div><div class="wa-split__side">{side}</div></div></form>"#,
        action = escape(&ctx.url("new")),
        csrf = escape(shell.csrf_token()),
    );
    shell.page_with(
        PageHead::new(t!("New template"))
            .back(t!("Message templates"), ctx.url(""))
            .assets(&[view::CSS, view::JS]),
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
