//! Ranklist prefetch: contest results once a contest is FINISHED, real ranks by a rating sweep (rules in `logic::schedule`).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cf::client::CodeforcesClient;
use crate::logic::schedule::{WINDOW, prefetch_due, rating_sweep_due};
use crate::services::Services;
use crate::store::base::Store;

/// Fetches and saves the contest's results for every linked handle (all guilds): the standings rows, then
/// `contest.status` for each linked handle the standings lack. A failing status lookup is logged and skipped.
/// Returns the number of results saved. Does not touch the done marker.
pub async fn fetch_and_save_results(cf: &CodeforcesClient, db: &Store, contest_id: i64, now: i64) -> anyhow::Result<usize> {
    let handles = db.all_linked_handles().await?;
    if handles.is_empty() {
        return Ok(0);
    }
    let mut results = cf.standings_results(contest_id, &handles).await?;
    let have: HashSet<String> = results.iter().map(|r| r.handle.to_lowercase()).collect();
    for h in handles.iter().filter(|h| !have.contains(&h.to_lowercase())) {
        match cf.status_result(contest_id, h).await {
            Ok(r) => results.extend(r),
            Err(e) => tracing::warn!("Prefetch status {contest_id}/{h} failed: {e:#}"),
        }
    }
    let n = results.len();
    db.upsert_contest_results(contest_id, results, now).await?;
    Ok(n)
}

/// One handle's `user.rating`, saved as real ranks (used when a handle is newly linked).
pub async fn fetch_and_save_ranks(cf: &CodeforcesClient, db: &Store, handle: &str, now: i64) -> anyhow::Result<()> {
    let ranks = cf.contest_ranks(handle).await?;
    db.upsert_contest_ranks(ranks.into_iter().map(|(c, r)| (c, handle.to_string(), r)).collect(), now).await
}

/// One prefetch + sweep round; each contest and handle fails alone.
pub async fn run_round(s: &Services, now: i64) {
    let mut sweep = vec![];
    for c in s.contests.contests().iter() {
        let Some(start) = c.start_time else { continue };
        let end = start + c.duration;
        if now - end > WINDOW {
            continue;
        }
        let st = match s.db.prefetch_state(c.id).await {
            Ok(st) => st,
            Err(e) => {
                tracing::error!("Prefetch state {} failed: {e:#}", c.id);
                continue;
            }
        };
        if prefetch_due(now, end, &c.phase, st.final_done) {
            match fetch_and_save_results(&s.cf, &s.db, c.id, now).await {
                Ok(_) => {
                    let st = crate::store::base::PrefetchState { final_done: true, last_fetch_at: Some(now), ..st.clone() };
                    if let Err(e) = s.db.set_prefetch_state(c.id, st).await {
                        tracing::error!("Prefetch marker {} failed: {e:#}", c.id);
                    }
                }
                Err(e) => tracing::error!("Prefetch of contest {} failed: {e:#}", c.id),
            }
        }
        if rating_sweep_due(now, end, st.ratings_found, st.last_rating_sweep_at) {
            sweep.push(c.id);
        }
    }
    if sweep.is_empty() {
        return;
    }
    let handles = match s.db.all_linked_handles().await {
        Ok(h) => h,
        Err(e) => return tracing::error!("Rating sweep handles failed: {e:#}"),
    };
    let mut found: HashSet<i64> = HashSet::new();
    for h in &handles {
        match s.cf.contest_ranks(h).await {
            Ok(all) => {
                let ranks: Vec<_> = all.into_iter().filter(|(c, _)| sweep.contains(c)).collect();
                found.extend(ranks.iter().map(|(c, _)| *c));
                if let Err(e) = s.db.upsert_contest_ranks(ranks.into_iter().map(|(c, r)| (c, h.clone(), r)).collect(), now).await {
                    tracing::error!("Rating sweep save {h} failed: {e:#}");
                }
            }
            Err(e) => tracing::warn!("Rating sweep {h} failed: {e:#}"),
        }
    }
    for id in sweep {
        // re-read: the prefetch above may have just written the marker
        let Ok(mut st) = s.db.prefetch_state(id).await else { continue };
        st.last_rating_sweep_at = Some(now);
        st.ratings_found |= found.contains(&id);
        if let Err(e) = s.db.set_prefetch_state(id, st).await {
            tracing::error!("Rating sweep state {id} failed: {e:#}");
        }
    }
}

/// Runs `run_round` in the background unless the previous round is still going.
pub fn spawn_round(s: Arc<Services>, busy: Arc<AtomicBool>, now: i64) {
    if busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    tokio::spawn(async move {
        run_round(&s, now).await;
        busy.store(false, Ordering::SeqCst);
    });
}
