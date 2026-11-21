//! Task priority and the WIP cap: overflow parking, promotion, rebalance.
//!
//! Why: a ledger holds at most `config.wip_cap` (default 8) active tasks; a new
//! task past the cap parks the worst one in the overflow queue and a closing
//! task promotes the best waiting one. The order must match `datg.py` exactly or
//! two writers would park different tasks.
//! What: [`wip_cap`], [`wip_count`], [`task_priority`], [`wip_check`],
//! [`promote_overflow`], [`rebalance`]. Tasks are touched by index so the
//! priority reads (which look at goals) never overlap a mutable borrow.
//! Test: `priority_inherits_from_goal_rank`, `add_past_cap_parks_the_worst_or_the_new_task`,
//! `promote_picks_best_priority_then_rank`, `rebalance_parks_and_promotes`.

use super::json::Json;

/// Default active-task cap.
pub const WIP_CAP_DEFAULT: i64 = 8;

fn active(t: &Json) -> bool {
    matches!(t.s("status"), "open" | "running")
}

/// `wip_cap`: `config.wip_cap` when a positive integer, else 8.
pub fn wip_cap(st: &Json) -> i64 {
    match st.get("config").and_then(|c| c.get("wip_cap")) {
        Some(Json::Int(n)) if *n > 0 => *n,
        _ => WIP_CAP_DEFAULT,
    }
}

/// Active tasks: open or running.
pub fn wip_count(st: &Json) -> i64 {
    st.arr("tasks").iter().filter(|t| active(t)).count() as i64
}

/// Effective priority 1-5: an explicit one, else from the goal rank (ad-hoc goal 4, no goal 5).
pub fn task_priority(st: &Json, t: &Json) -> i64 {
    if let Some(Json::Int(p)) = t.get("priority")
        && (1..=5).contains(p)
    {
        return *p;
    }
    let goal = st.arr("goals").iter().find(|g| g.s("id") == t.s("goal"));
    let Some(g) = goal else { return 5 };
    if g.s("title").starts_with("Handle ad-hoc tasks") {
        return 4;
    }
    match g.i("rank").unwrap_or(0) {
        ..=1 => 2,
        2..=3 => 3,
        4..=6 => 4,
        _ => 5,
    }
}

fn activity(t: &Json) -> &str {
    ["last_signal_at", "last_msg_at", "created"]
        .iter()
        .map(|k| t.s(k))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// Sort key, worst first: lowest priority (highest number), then oldest activity.
fn drop_key<'a>(st: &Json, t: &'a Json) -> (i64, &'a str, &'a str) {
    (-task_priority(st, t), activity(t), t.s("id"))
}

fn park(t: &mut Json) {
    t.set("status", "parked");
    t.set("overflow", true);
}

fn tasks_mut(st: &mut Json) -> &mut Vec<Json> {
    st.arr_or_insert("tasks")
}

/// `(-priority, is not the new task, activity, id)`: the smallest is parked first.
type DropRank = (i64, bool, String, String);

/// A task added past the cap sends the lowest-priority one (ties: oldest activity, then the new
/// task) to the overflow queue. `obj` is the new task, not yet in `st`. Priority 1 is never parked.
pub fn wip_check(st: &mut Json, new_id: &str, obj: &mut Json, warnings: &mut Vec<String>) {
    let (n, cap) = (wip_count(st) + 1, wip_cap(st));
    if n <= cap {
        return;
    }
    // (key, index in tasks; None = the new task). A tie drops the new task first.
    let mut cand: Vec<(DropRank, Option<usize>)> = Vec::new();
    for (i, t) in st
        .arr("tasks")
        .iter()
        .enumerate()
        .filter(|(_, t)| active(t))
    {
        if task_priority(st, t) != 1 {
            let k = drop_key(st, t);
            cand.push(((k.0, true, k.1.to_owned(), k.2.to_owned()), Some(i)));
        }
    }
    if task_priority(st, obj) != 1 {
        let k = drop_key(st, obj);
        cand.push(((k.0, false, k.1.to_owned(), k.2.to_owned()), None));
    }
    let Some((_, victim)) = cand.into_iter().min_by(|a, b| a.0.cmp(&b.0)) else {
        warnings.push(format!(
            "note: {new_id} opened past the cap ({} active tasks, cap {cap}): all active tasks are priority 1",
            n - 1
        ));
        return;
    };
    let vid = match victim {
        Some(i) => {
            let t = &mut tasks_mut(st)[i];
            park(t);
            t.s("id").to_owned()
        }
        None => {
            park(obj);
            obj.s("id").to_owned()
        }
    };
    let vid = if vid.is_empty() {
        new_id.to_owned()
    } else {
        vid
    };
    warnings.push(format!(
        "note: {vid} went to the overflow queue (parked) - {} active tasks, cap {cap}",
        n - 1
    ));
}

fn overflow_queue(st: &Json) -> Vec<usize> {
    let truthy = |t: &Json| t.get("overflow").is_some_and(Json::truthy);
    (0..st.arr("tasks").len())
        .filter(|&i| {
            let t = &st.arr("tasks")[i];
            truthy(t) && t.s("status") == "parked"
        })
        .collect()
}

/// While under the cap, opens the overflow task of the best priority (ties: goal rank, oldest, id).
pub fn promote_overflow(st: &mut Json) -> Vec<String> {
    let mut moved = Vec::new();
    while wip_count(st) < wip_cap(st) {
        let q = overflow_queue(st);
        let best = q.into_iter().min_by(|&a, &b| {
            let key = |i: usize| {
                let t = &st.arr("tasks")[i];
                let rank = st
                    .arr("goals")
                    .iter()
                    .find(|g| g.s("id") == t.s("goal"))
                    .and_then(|g| g.i("rank"))
                    .unwrap_or(1_000_000);
                (
                    task_priority(st, t),
                    rank,
                    t.s("created").to_owned(),
                    t.s("id").to_owned(),
                )
            };
            key(a).cmp(&key(b))
        });
        let Some(i) = best else { break };
        let t = &mut tasks_mut(st)[i];
        t.set("status", "open");
        t.remove("overflow");
        moved.push(t.s("id").to_owned());
    }
    moved
}

fn worst_active(st: &Json) -> Option<usize> {
    let tasks = st.arr("tasks");
    (0..tasks.len())
        .filter(|&i| active(&tasks[i]) && task_priority(st, &tasks[i]) != 1)
        .min_by(|&a, &b| drop_key(st, &tasks[a]).cmp(&drop_key(st, &tasks[b])))
}

/// Re-runs the cap by priority; returns `(id, change)` pairs.
pub fn rebalance(st: &mut Json) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    while wip_count(st) > wip_cap(st) {
        let Some(v) = worst_active(st) else { break };
        let t = &mut tasks_mut(st)[v];
        park(t);
        out.push((t.s("id").to_owned(), "parked to overflow"));
    }
    out.extend(
        promote_overflow(st)
            .into_iter()
            .map(|id| (id, "promoted from overflow")),
    );
    loop {
        let q = overflow_queue(st);
        let (Some(worst), false) = (worst_active(st), q.is_empty()) else {
            break;
        };
        let tasks = st.arr("tasks");
        let best = q
            .into_iter()
            .min_by(|&a, &b| {
                let k = |i: usize| {
                    (
                        task_priority(st, &tasks[i]),
                        tasks[i].s("created"),
                        tasks[i].s("id"),
                    )
                };
                k(a).cmp(&k(b))
            })
            .unwrap_or(worst);
        if task_priority(st, &tasks[best]) >= task_priority(st, &tasks[worst]) {
            break;
        }
        let list = tasks_mut(st);
        park(&mut list[worst]);
        list[best].set("status", "open");
        list[best].remove("overflow");
        out.push((list[worst].s("id").to_owned(), "parked to overflow"));
        out.push((list[best].s("id").to_owned(), "promoted from overflow"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::json::obj;

    fn task(id: &str, goal: &str, status: &str, created: &str) -> Json {
        obj([
            ("id", id.into()),
            ("goal", goal.into()),
            ("title", id.into()),
            ("status", status.into()),
            ("created", created.into()),
        ])
    }

    fn ledger(cap: i64, tasks: Vec<Json>) -> Json {
        let goal = |id: &str, rank: i64, title: &str| {
            obj([
                ("id", id.into()),
                ("rank", rank.into()),
                ("title", title.into()),
                ("status", "active".into()),
            ])
        };
        obj([
            ("code", "tt".into()),
            ("config", obj([("wip_cap", cap.into())])),
            (
                "goals",
                Json::Arr(vec![
                    goal("tt-G01", 1, "top"),
                    goal("tt-G02", 8, "low"),
                    goal("tt-G03", 2, "Handle ad-hoc tasks"),
                ]),
            ),
            ("tasks", Json::Arr(tasks)),
        ])
    }

    #[test]
    fn priority_inherits_from_goal_rank() {
        let st = ledger(8, vec![]);
        assert_eq!(task_priority(&st, &task("a", "tt-G01", "open", "")), 2);
        assert_eq!(task_priority(&st, &task("a", "tt-G02", "open", "")), 5);
        assert_eq!(task_priority(&st, &task("a", "tt-G03", "open", "")), 4);
        assert_eq!(task_priority(&st, &task("a", "none", "open", "")), 5);
        let mut t = task("a", "tt-G02", "open", "");
        t.set("priority", 1);
        assert_eq!(task_priority(&st, &t), 1);
    }

    #[test]
    fn add_past_cap_parks_the_worst_or_the_new_task() {
        let mut st = ledger(
            2,
            vec![
                task("tt-T01", "tt-G01", "open", "2026-01-01"),
                task("tt-T02", "tt-G02", "open", "2026-01-02"),
            ],
        );
        let mut w = Vec::new();
        let mut new = task("tt-T03", "tt-G01", "open", "2026-01-03");
        wip_check(&mut st, "tt-T03", &mut new, &mut w);
        assert_eq!(st.arr("tasks")[1].s("status"), "parked");
        assert_eq!(st.arr("tasks")[1].get("overflow"), Some(&Json::Bool(true)));
        assert!(
            w[0].starts_with("note: tt-T02 went to the overflow queue"),
            "{w:?}"
        );
        // A tie parks the new task.
        let mut st = ledger(1, vec![task("tt-T01", "tt-G01", "open", "2026-01-01")]);
        let mut new = task("tt-T02", "tt-G01", "open", "2026-01-02");
        wip_check(&mut st, "tt-T02", &mut new, &mut Vec::new());
        assert_eq!(new.s("status"), "parked");
    }

    #[test]
    fn promote_picks_best_priority_then_rank() {
        let mut a = task("tt-T01", "tt-G02", "parked", "2026-01-01");
        a.set("overflow", true);
        let mut b = task("tt-T02", "tt-G01", "parked", "2026-01-02");
        b.set("overflow", true);
        let mut st = ledger(1, vec![a, b]);
        assert_eq!(promote_overflow(&mut st), vec!["tt-T02"]);
        assert!(st.arr("tasks")[1].get("overflow").is_none());
        assert_eq!(st.arr("tasks")[0].s("status"), "parked");
    }

    #[test]
    fn rebalance_parks_and_promotes() {
        let mut st = ledger(
            1,
            vec![
                task("tt-T01", "tt-G02", "open", "2026-01-01"),
                task("tt-T02", "tt-G01", "open", "2026-01-02"),
            ],
        );
        let out = rebalance(&mut st);
        assert_eq!(out, vec![("tt-T01".to_owned(), "parked to overflow")]);
        let mut st = ledger(1, vec![task("tt-T01", "tt-G02", "open", "2026-01-01")]);
        st.arr_mut("tasks").unwrap().push({
            let mut t = task("tt-T02", "tt-G01", "parked", "2026-01-02");
            t.set("overflow", true);
            t
        });
        let out = rebalance(&mut st);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(st.arr("tasks")[1].s("status"), "open");
    }
}
