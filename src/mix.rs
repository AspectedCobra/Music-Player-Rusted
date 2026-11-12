use crate::model::*;
use crate::ytm;
use rand::{distributions::WeightedIndex, prelude::*};
use std::collections::HashSet;
use std::path::Path;

fn names(t: &Track) -> HashSet<String> {
    split_artists(&t.artist).into_iter().map(|n| n.to_lowercase()).collect()
}
fn lead(t: &Track) -> String {
    split_artists(&t.artist).first().map(|n| n.to_lowercase()).unwrap_or_default()
}

struct Entry {
    cands: Vec<Track>,
    cap: usize,
    taken: usize,
}

/// Pick the next songs: NEW music in the spirit of your taste.
/// - several 'seeds' (the current song plus songs you like, each from a different artist);
/// - never a song heard before, never an artist you keep skipping;
/// - at most one song per artist in a batch, none by the artists of the last few songs;
/// - songs are taken from the seeds in turns, in random order.
pub fn build_mix(
    current: &Track,
    avoid_ids: &HashSet<String>,
    avoid_keys: &HashSet<String>,
    recent: &HashSet<String>,
    taste_seeds: Vec<Track>,
    scores: &Scores,
    size: usize,
) -> Vec<Track> {
    let mut rng = thread_rng();
    let mut others = taste_seeds;
    others.shuffle(&mut rng);
    let mut seeds: Vec<(Track, usize)> = vec![(current.clone(), 3)]; // the current song steers less
    let mut used: HashSet<String> = HashSet::from([lead(current)]);
    for t in others {
        if seeds.len() >= 5 {
            break;
        }
        if t.id != current.id && !used.contains(&lead(&t)) && scores.affinity(&t.artist) > -2.0 {
            used.insert(lead(&t));
            seeds.push((t, 5));
        }
    }

    let fetched: Vec<Vec<Track>> = std::thread::scope(|s| {
        let handles: Vec<_> = seeds.iter().map(|(t, _)| s.spawn(move || ytm::radio(t, 50).unwrap_or_default())).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });

    let usable = |t: &Track, strict: bool| -> bool {
        if avoid_ids.contains(&t.id) || avoid_keys.contains(&song_key(&t.title, &t.artist)) {
            return false;
        }
        if scores.affinity(&t.artist) <= -2.0 {
            return false;
        }
        !(strict && !names(t).is_disjoint(recent))
    };

    let mut picked: Vec<Track> = vec![];
    let (mut got_ids, mut got_keys, mut got_names): (HashSet<String>, HashSet<String>, HashSet<String>) = Default::default();
    for strict in [true, false] {
        // relax only if too little is left
        if picked.len() >= 5 {
            break;
        }
        let mut lists: Vec<Entry> = seeds.iter().zip(&fetched).map(|((_, cap), songs)| {
            let mut cands: Vec<Track> = songs.iter().filter(|t| usable(t, strict)).cloned().collect();
            cands.shuffle(&mut rng);
            Entry { cands, cap: *cap, taken: 0 }
        }).collect();
        lists.shuffle(&mut rng);
        let mut progressed = true;
        while progressed && picked.len() < size {
            progressed = false;
            for e in lists.iter_mut() {
                loop {
                    if e.cands.is_empty() || e.taken >= e.cap || picked.len() >= size {
                        break;
                    }
                    let t = e.cands.pop().unwrap();
                    let key = song_key(&t.title, &t.artist);
                    if got_ids.contains(&t.id) || got_keys.contains(&key) || !names(&t).is_disjoint(&got_names) {
                        continue;
                    }
                    got_ids.insert(t.id.clone());
                    got_keys.insert(key);
                    got_names.extend(names(&t));
                    picked.push(t);
                    e.taken += 1;
                    progressed = true;
                    break;
                }
            }
        }
    }
    picked
}

/// One random song from your liking, skipping `exclude` ids, artists you keep skipping
/// and saved files that no longer exist.
pub fn pick_liked(liked: &[(Track, f64)], exclude: &HashSet<String>, scores: &Scores) -> Option<Track> {
    let options: Vec<&(Track, f64)> = liked.iter()
        .filter(|(t, _)| !exclude.contains(&t.id) && scores.affinity(&t.artist) > -2.0)
        .filter(|(t, _)| t.id.strip_prefix("local:").map(|p| Path::new(p).exists()).unwrap_or(true))
        .collect();
    let dist = WeightedIndex::new(options.iter().map(|(_, w)| w.max(0.01))).ok()?;
    Some(options[dist.sample(&mut thread_rng())].0.clone())
}

/// Songs from your saved folder for autoplay when YouTube is unreachable.
pub fn pick_offline(library: &[Track], avoid_ids: &HashSet<String>, avoid_keys: &HashSet<String>, scores: &Scores, size: usize) -> Vec<Track> {
    let mut pool: Vec<Track> = library.iter()
        .filter(|t| !avoid_ids.contains(&t.id) && !avoid_keys.contains(&song_key(&t.title, &t.artist)) && scores.affinity(&t.artist) > -2.0)
        .cloned().collect();
    pool.shuffle(&mut thread_rng());
    pool.truncate(size);
    pool
}
