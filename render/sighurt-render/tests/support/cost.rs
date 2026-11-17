//! A probe of what a renderer costs its process, for the GPU backends' `cost` measurements
//! (included by path): resident memory (`VmRSS`, `VmHWM`), threads and time at named stages,
//! and the largest resident mappings grouped by file. Linux's `/proc` only.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Instant;

pub struct Probe {
    name: &'static str,
    started: Instant,
    last: Instant,
}

impl Probe {
    pub fn new(name: &'static str) -> Self {
        let now = Instant::now();
        Self {
            name,
            started: now,
            last: now,
        }
    }

    /// One line: the stage, its time, the time so far, resident memory and its peak, threads.
    pub fn stage(&mut self, stage: &str) {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let field = |key: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|v| v.trim().to_owned())
                .unwrap_or_default()
        };
        let now = Instant::now();
        println!(
            "cost {} {stage:?}: {:.1} ms (total {:.1} ms), VmRSS {}, VmHWM {}, threads {}",
            self.name,
            (now - self.last).as_secs_f64() * 1e3,
            (now - self.started).as_secs_f64() * 1e3,
            field("VmRSS:"),
            field("VmHWM:"),
            field("Threads:"),
        );
        self.last = now;
    }

    /// The `top` mappings with the most resident memory, by file (anonymous ones together).
    pub fn mappings(&self, top: usize) {
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
        let mut rss: BTreeMap<String, u64> = BTreeMap::new();
        let mut name = String::new();
        for line in smaps.lines() {
            let mut fields = line.split_whitespace();
            let first = fields.next().unwrap_or_default();
            if first.contains('-') && first.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                name = fields.nth(4).unwrap_or("[anon]").to_owned();
            } else if first == "Rss:" {
                let kb: u64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                *rss.entry(name.clone()).or_default() += kb;
            }
        }
        let mut sorted: Vec<_> = rss.into_iter().filter(|(_, kb)| *kb > 0).collect();
        sorted.sort_by_key(|(_, kb)| std::cmp::Reverse(*kb));
        let total: u64 = sorted.iter().map(|(_, kb)| kb).sum();
        println!("cost {} mappings: {total} kB resident in all", self.name);
        for (name, kb) in sorted.iter().take(top) {
            println!("cost {}   {kb:>8} kB {name}", self.name);
        }
    }
}
