/*
 *
 *       user/desktop/src/apps/mines.rs
 *       SPUI 4.0 Classic Windows XP Smart Minesweeper
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Classic Windows XP (`winmine.exe`) Minesweeper with an integrated
//! Constraint-Satisfaction & Binomial-Weighted AI assistant.
//!
//! Visuals faithfully reproduce the `#C0C0C0` 3D beveled chassis, recessed black
//! red-LED 7-segment mine counter and timer (`-99..=999`), interactive 3D smiley
//! reset button (`Normal`, `Surprised`, `Won`, `Lost`), classic 1–8 digit colors,
//! and right-click flag/`?` cycling plus chording.
//!
//! The AI engine executes:
//! 1. Single-cell counting & pairwise subset subtraction / pigeonhole bounds.
//! 2. Connected-component decomposition with exact Tank CSP backtracking (`<=22`
//!    frontier variables per component) and log-domain binomial global mine
//!    budget weighting.
//! 3. Local constraint approximation fallback for huge (>22-cell) components
//!    and information-gain / corner tie-breaking on equal probabilities.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::sound::{play as play_sound, SystemSound};
use crate::text::{Face, TextRenderer};
use crate::theme::TITLEBAR_HEIGHT;
use crate::x11::{CursorKind, Result};
use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::ConnectionExt;

// Classic Windows XP palette
const XP_FACE: u32 = 0xc0c0c0;
const XP_LIGHT: u32 = 0xffffff;
const XP_SHADOW: u32 = 0x808080;
const XP_DARK: u32 = 0x404040;
const XP_BLACK: u32 = 0x000000;
const XP_LED_ON: u32 = 0xff0000;
const XP_LED_OFF: u32 = 0x3a0000;
const XP_RED: u32 = 0xff0000;
const XP_YELLOW: u32 = 0xffff00;
const XP_TOOLBAR_BG: u32 = 0xd4d0c8;
const XP_TOOLBAR_ACTIVE: u32 = 0x316ac5;
const XP_STATUS_BG: u32 = 0xd4d0c8;

const NUMBER_COLORS: [u32; 9] = [
    XP_FACE,  // 0
    0x0000ff, // 1: blue
    0x008000, // 2: green
    0xff0000, // 3: red
    0x000080, // 4: navy
    0x800000, // 5: maroon
    0x008080, // 6: teal
    0x000000, // 7: black
    0x808080, // 8: grey
];

const TANK_EXACT_VAR_LIMIT: usize = 22;
const TANK_MAX_NODES: usize = 1_500_000;
const AUTO_STEP_MS: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difficulty {
    Beginner,
    Intermediate,
    Expert,
    Custom(usize, usize, usize),
}

impl Difficulty {
    pub fn dims(self) -> (usize, usize, usize) {
        match self {
            Self::Beginner => (9, 9, 10),
            Self::Intermediate => (16, 16, 40),
            Self::Expert => (30, 16, 99),
            Self::Custom(w, h, m) => {
                let cw = w.clamp(9, 30);
                let ch = h.clamp(9, 24);
                let max_m = (cw * ch).saturating_sub(1).max(1);
                (cw, ch, m.clamp(1, max_m))
            }
        }
    }

    fn preferred_cell_size(self) -> i32 {
        let (w, _, _) = self.dims();
        if w <= 9 {
            28
        } else if w <= 16 {
            24
        } else {
            22
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkState {
    Hidden,
    Flagged,
    Question,
    Revealed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameStatus {
    Ready,
    Playing,
    Won,
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SmileyFace {
    Normal,
    Surprised,
    Won,
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiActionKind {
    Reveal,
    Flag,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AiRecommendation {
    pub cell: usize,
    pub action: AiActionKind,
    pub mine_prob: f64,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct AiSnapshot {
    pub probs: Vec<Option<f64>>,
    pub reasons: Vec<String>,
    pub safe_cells: Vec<usize>,
    pub mine_cells: Vec<usize>,
    pub best_move: Option<AiRecommendation>,
    pub exact_global: bool,
    pub summary: String,
}

impl Default for AiSnapshot {
    fn default() -> Self {
        Self {
            probs: Vec::new(),
            reasons: Vec::new(),
            safe_cells: Vec::new(),
            mine_cells: Vec::new(),
            best_move: None,
            exact_global: true,
            summary: String::from("左键首击开局（首击 3×3 九宫格必安全）"),
        }
    }
}

struct Rng64 {
    state: u64,
}

impl Rng64 {
    fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64);
        Self::from_seed(nanos ^ 0x517c_c1b7_2722_0a95)
    }

    fn from_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            (self.next_u64() % upper as u64) as usize
        }
    }
}

pub struct MinesBoard {
    pub cols: usize,
    pub rows: usize,
    pub total_mines: usize,
    pub zero_guess_mode: bool,
    pub mines: Vec<bool>,
    pub adj: Vec<u8>,
    pub marks: Vec<MarkState>,
    pub exploded_cell: Option<usize>,
    pub mines_placed: bool,
    pub status: GameStatus,
    pub revealed_safe: usize,
    pub flags_placed: usize,
    pub started_at: Option<Instant>,
    pub final_seconds: u32,
    rng: Rng64,
}

impl MinesBoard {
    pub fn new(cols: usize, rows: usize, total_mines: usize, zero_guess_mode: bool) -> Self {
        let len = cols * rows;
        Self {
            cols,
            rows,
            total_mines: total_mines.min(len.saturating_sub(1).max(1)),
            zero_guess_mode,
            mines: vec![false; len],
            adj: vec![0; len],
            marks: vec![MarkState::Hidden; len],
            exploded_cell: None,
            mines_placed: false,
            status: GameStatus::Ready,
            revealed_safe: 0,
            flags_placed: 0,
            started_at: None,
            final_seconds: 0,
            rng: Rng64::from_clock(),
        }
    }

    #[cfg(test)]
    fn from_preset(cols: usize, rows: usize, mines_at: &[usize]) -> Self {
        let mut b = Self::new(cols, rows, mines_at.len(), false);
        for &idx in mines_at {
            b.mines[idx] = true;
        }
        b.recompute_adj();
        b.mines_placed = true;
        b.status = GameStatus::Playing;
        b
    }

    pub fn len(&self) -> usize {
        self.cols * self.rows
    }

    pub fn idx(&self, x: usize, y: usize) -> usize {
        y * self.cols + x
    }

    pub fn coords(&self, idx: usize) -> (usize, usize) {
        (idx % self.cols, idx / self.cols)
    }

    pub fn neighbors(&self, idx: usize) -> Vec<usize> {
        let (x, y) = self.coords(idx);
        let mut out = Vec::with_capacity(8);
        for dy in -1i32..=1 {
            let ny = y as i32 + dy;
            if ny < 0 || ny >= self.rows as i32 {
                continue;
            }
            for dx in -1i32..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let nx = x as i32 + dx;
                if nx >= 0 && nx < self.cols as i32 {
                    out.push(self.idx(nx as usize, ny as usize));
                }
            }
        }
        out
    }

    pub fn elapsed_seconds(&self) -> u32 {
        match self.status {
            GameStatus::Ready => 0,
            GameStatus::Playing => self
                .started_at
                .map_or(0, |t| (t.elapsed().as_secs() as u32).min(999)),
            GameStatus::Won | GameStatus::Lost => self.final_seconds.min(999),
        }
    }

    pub fn remaining_mines_display(&self) -> i32 {
        (self.total_mines as i32 - self.flags_placed as i32).clamp(-99, 999)
    }

    fn recompute_adj(&mut self) {
        for i in 0..self.len() {
            if self.mines[i] {
                self.adj[i] = 0;
            } else {
                let cnt = self
                    .neighbors(i)
                    .into_iter()
                    .filter(|&n| self.mines[n])
                    .count() as u8;
                self.adj[i] = cnt;
            }
        }
    }

    fn is_in_safe_zone(&self, center: usize, candidate: usize, radius: usize) -> bool {
        let (cx, cy) = self.coords(center);
        let (x, y) = self.coords(candidate);
        cx.abs_diff(x) <= radius && cy.abs_diff(y) <= radius
    }

    fn place_random_mines(&mut self, first_idx: usize) {
        let len = self.len();
        let safe9_count = (0..len)
            .filter(|&i| self.is_in_safe_zone(first_idx, i, 1))
            .count();
        let radius = if len.saturating_sub(safe9_count) >= self.total_mines {
            1
        } else {
            0
        };

        let mut pool: Vec<usize> = (0..len)
            .filter(|&i| !self.is_in_safe_zone(first_idx, i, radius))
            .collect();
        // Fisher-Yates prefix shuffle
        let need = self.total_mines.min(pool.len());
        self.mines.fill(false);
        for i in 0..need {
            let pick = i + self.rng.gen_range(pool.len() - i);
            pool.swap(i, pick);
            self.mines[pool[i]] = true;
        }
        self.recompute_adj();
        self.mines_placed = true;
    }

    fn place_mines_with_preference(&mut self, first_idx: usize) {
        if !self.zero_guess_mode {
            self.place_random_mines(first_idx);
            return;
        }

        // Try generating candidate boards solvable without guessing (deterministic
        // pass + CSP zero-probability certainties). Keep the candidate that clears
        // the most safe tiles deterministically if a dense custom ratio fails.
        let attempts = if self.len() <= 100 {
            180
        } else if self.len() <= 260 {
            90
        } else {
            36
        };
        let mut best_mines = Vec::new();
        let mut best_progress = 0usize;

        for _ in 0..attempts {
            self.place_random_mines(first_idx);
            let (solved, progress) = test_board_solvable_no_guess(
                self.cols,
                self.rows,
                self.total_mines,
                &self.mines,
                &self.adj,
                first_idx,
            );
            if solved {
                return;
            }
            if progress >= best_progress {
                best_progress = progress;
                best_mines = self.mines.clone();
            }
        }
        if !best_mines.is_empty() {
            self.mines = best_mines;
            self.recompute_adj();
        }
    }

    /// Reveal `idx`. Returns `true` if board visual state changed.
    pub fn reveal(&mut self, idx: usize) -> bool {
        if matches!(self.status, GameStatus::Won | GameStatus::Lost) {
            return false;
        }
        if matches!(self.marks[idx], MarkState::Flagged | MarkState::Revealed) {
            return false;
        }

        if !self.mines_placed {
            self.place_mines_with_preference(idx);
            self.status = GameStatus::Playing;
            self.started_at = Some(Instant::now());
        } else if self.status == GameStatus::Ready {
            self.status = GameStatus::Playing;
            self.started_at = Some(Instant::now());
        }

        if self.mines[idx] {
            self.marks[idx] = MarkState::Revealed;
            self.exploded_cell = Some(idx);
            self.finish_lost();
            return true;
        }

        self.flood_reveal(idx);
        self.check_win();
        true
    }

    fn flood_reveal(&mut self, start: usize) {
        let mut queue = VecDeque::new();
        queue.push_back(start);
        while let Some(cur) = queue.pop_front() {
            if self.marks[cur] == MarkState::Revealed || self.marks[cur] == MarkState::Flagged {
                continue;
            }
            if self.mines[cur] {
                continue;
            }
            self.marks[cur] = MarkState::Revealed;
            self.revealed_safe += 1;
            if self.adj[cur] == 0 {
                for n in self.neighbors(cur) {
                    if matches!(self.marks[n], MarkState::Hidden | MarkState::Question) {
                        queue.push_back(n);
                    }
                }
            }
        }
    }

    pub fn cycle_mark(&mut self, idx: usize) -> bool {
        if matches!(self.status, GameStatus::Won | GameStatus::Lost) {
            return false;
        }
        match self.marks[idx] {
            MarkState::Hidden => {
                self.marks[idx] = MarkState::Flagged;
                self.flags_placed += 1;
                true
            }
            MarkState::Flagged => {
                self.marks[idx] = MarkState::Question;
                self.flags_placed = self.flags_placed.saturating_sub(1);
                true
            }
            MarkState::Question => {
                self.marks[idx] = MarkState::Hidden;
                true
            }
            MarkState::Revealed => false,
        }
    }

    pub fn set_flag(&mut self, idx: usize, flag: bool) -> bool {
        if matches!(self.status, GameStatus::Won | GameStatus::Lost) {
            return false;
        }
        match (self.marks[idx], flag) {
            (MarkState::Hidden | MarkState::Question, true) => {
                self.marks[idx] = MarkState::Flagged;
                self.flags_placed += 1;
                true
            }
            (MarkState::Flagged, false) => {
                self.marks[idx] = MarkState::Hidden;
                self.flags_placed = self.flags_placed.saturating_sub(1);
                true
            }
            _ => false,
        }
    }

    /// Middle click or simultaneous L+R chord on revealed cell `idx`.
    pub fn chord(&mut self, idx: usize) -> bool {
        if self.status != GameStatus::Playing || self.marks[idx] != MarkState::Revealed {
            return false;
        }
        let target = self.adj[idx] as usize;
        if target == 0 {
            return false;
        }
        let nbrs = self.neighbors(idx);
        let flagged = nbrs
            .iter()
            .filter(|&&n| self.marks[n] == MarkState::Flagged)
            .count();
        if flagged != target {
            return false;
        }
        let mut changed = false;
        let mut hit_mine: Option<usize> = None;
        for n in nbrs {
            if matches!(self.marks[n], MarkState::Hidden | MarkState::Question) {
                if self.mines[n] {
                    if hit_mine.is_none() {
                        hit_mine = Some(n);
                    }
                    self.marks[n] = MarkState::Revealed;
                    changed = true;
                } else {
                    self.flood_reveal(n);
                    changed = true;
                }
            }
        }
        if let Some(m) = hit_mine {
            self.exploded_cell = Some(m);
            self.finish_lost();
        } else {
            self.check_win();
        }
        changed
    }

    fn finish_lost(&mut self) {
        self.final_seconds = self.elapsed_seconds();
        self.status = GameStatus::Lost;
    }

    fn check_win(&mut self) {
        let safe_total = self.len().saturating_sub(self.total_mines);
        if self.revealed_safe >= safe_total {
            self.final_seconds = self.elapsed_seconds();
            self.status = GameStatus::Won;
            // XP win condition auto-flags remaining unflagged mines!
            for i in 0..self.len() {
                if self.mines[i] && self.marks[i] != MarkState::Flagged {
                    self.marks[i] = MarkState::Flagged;
                    self.flags_placed += 1;
                }
            }
        }
    }
}

/// Fast headless solver simulation used by Zero-Guess board generation.
fn test_board_solvable_no_guess(
    cols: usize,
    rows: usize,
    total_mines: usize,
    mines: &[bool],
    adj: &[u8],
    first_idx: usize,
) -> (bool, usize) {
    let mut sim = MinesBoard::new(cols, rows, total_mines, false);
    sim.mines.copy_from_slice(mines);
    sim.adj.copy_from_slice(adj);
    sim.mines_placed = true;
    sim.status = GameStatus::Playing;
    sim.reveal(first_idx);

    let max_steps = cols * rows + 8;
    for _ in 0..max_steps {
        if sim.status == GameStatus::Won {
            return (true, sim.revealed_safe);
        }
        let ai = analyze_board(&sim);
        let mut progressed = false;
        for &m in &ai.mine_cells {
            if sim.set_flag(m, true) {
                progressed = true;
            }
        }
        for &s in &ai.safe_cells {
            if sim.reveal(s) {
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    (sim.status == GameStatus::Won, sim.revealed_safe)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Constraint {
    vars: Vec<usize>,
    target: usize,
}

impl Constraint {
    fn normalized(mut vars: Vec<usize>, target: usize) -> Option<Self> {
        vars.sort_unstable();
        vars.dedup();
        if target > vars.len() {
            return None;
        }
        Some(Self { vars, target })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CellDeduction {
    Unknown,
    Safe,
    Mine,
}

/// Log-space addition ln(e^a + e^b).
fn log_add(a: f64, b: f64) -> f64 {
    if a == f64::NEG_INFINITY {
        return b;
    }
    if b == f64::NEG_INFINITY {
        return a;
    }
    let hi = a.max(b);
    let lo = a.min(b);
    hi + (lo - hi).exp().ln_1p()
}

struct LogBinomial {
    ln_fact: Vec<f64>,
}

impl LogBinomial {
    fn new(max_n: usize) -> Self {
        let mut ln_fact = Vec::with_capacity(max_n + 1);
        ln_fact.push(0.0);
        let mut acc = 0.0f64;
        for i in 1..=max_n {
            acc += (i as f64).ln();
            ln_fact.push(acc);
        }
        Self { ln_fact }
    }

    fn ln_comb(&self, n: usize, k: usize) -> f64 {
        if k > n || n >= self.ln_fact.len() {
            return f64::NEG_INFINITY;
        }
        self.ln_fact[n] - self.ln_fact[k] - self.ln_fact[n - k]
    }
}

struct TankCompResult {
    vars: Vec<usize>,
    /// ways_by_k[k] = exact valid configurations of this component having k mines.
    ways_by_k: Vec<f64>,
    /// var_mine_ways[local_idx][k] = valid configs where `vars[local_idx]` is a mine.
    var_mine_ways: Vec<Vec<f64>>,
    exact: bool,
}

struct DfsComponentSolver {
    var_count: usize,
    targets: Vec<usize>,
    var_to_cons: Vec<Vec<usize>>,
    assign: Vec<u8>,
    con_mines: Vec<usize>,
    con_unassigned: Vec<usize>,
    ways_by_k: Vec<f64>,
    var_mine_ways: Vec<Vec<f64>>,
    nodes: usize,
    aborted: bool,
}

impl DfsComponentSolver {
    fn new(var_count: usize, constraints: &[Vec<usize>], targets: Vec<usize>) -> Self {
        let mut var_to_cons = vec![Vec::new(); var_count];
        let mut con_unassigned = Vec::with_capacity(constraints.len());
        for (ci, vars) in constraints.iter().enumerate() {
            con_unassigned.push(vars.len());
            for &vi in vars {
                var_to_cons[vi].push(ci);
            }
        }
        Self {
            var_count,
            targets,
            var_to_cons,
            assign: vec![0; var_count],
            con_mines: vec![0; constraints.len()],
            con_unassigned,
            ways_by_k: vec![0.0; var_count + 1],
            var_mine_ways: vec![vec![0.0; var_count + 1]; var_count],
            nodes: 0,
            aborted: false,
        }
    }

    fn solve(&mut self, depth: usize, mines_so_far: usize) {
        if self.aborted {
            return;
        }
        self.nodes += 1;
        if self.nodes > TANK_MAX_NODES {
            self.aborted = true;
            return;
        }
        if depth == self.var_count {
            self.ways_by_k[mines_so_far] += 1.0;
            for vi in 0..self.var_count {
                if self.assign[vi] == 1 {
                    self.var_mine_ways[vi][mines_so_far] += 1.0;
                }
            }
            return;
        }

        // Unassign marker update happens around recursion for depth var
        for &ci in &self.var_to_cons[depth] {
            self.con_unassigned[ci] -= 1;
        }

        for val in 0u8..=1u8 {
            let mut ok = true;
            if val == 1 {
                for &ci in &self.var_to_cons[depth] {
                    self.con_mines[ci] += 1;
                }
            }
            for &ci in &self.var_to_cons[depth] {
                let m = self.con_mines[ci];
                let rem = self.con_unassigned[ci];
                let tgt = self.targets[ci];
                if m > tgt || m + rem < tgt {
                    ok = false;
                    break;
                }
            }
            if ok {
                self.assign[depth] = val;
                self.solve(depth + 1, mines_so_far + val as usize);
                if self.aborted {
                    if val == 1 {
                        for &ci in &self.var_to_cons[depth] {
                            self.con_mines[ci] -= 1;
                        }
                    }
                    break;
                }
            }
            if val == 1 {
                for &ci in &self.var_to_cons[depth] {
                    self.con_mines[ci] -= 1;
                }
            }
        }

        for &ci in &self.var_to_cons[depth] {
            self.con_unassigned[ci] += 1;
        }
    }
}

fn solve_component_tank(comp_vars: &[usize], comp_cons: &[&Constraint]) -> TankCompResult {
    let n = comp_vars.len();
    if n > TANK_EXACT_VAR_LIMIT {
        return TankCompResult {
            vars: comp_vars.to_vec(),
            ways_by_k: Vec::new(),
            var_mine_ways: Vec::new(),
            exact: false,
        };
    }

    // Order variables by descending constraint degree to tighten DFS pruning.
    let mut order: Vec<usize> = (0..n).collect();
    let mut deg = vec![0usize; n];
    for c in comp_cons {
        for &v in &c.vars {
            if let Ok(pos) = comp_vars.binary_search(&v) {
                deg[pos] += 1;
            }
        }
    }
    order.sort_by(|&a, &b| deg[b].cmp(&deg[a]).then_with(|| a.cmp(&b)));

    let ordered_global: Vec<usize> = order.iter().map(|&orig| comp_vars[orig]).collect();
    let mut local_of_global = Vec::with_capacity(n);
    for (local, &g) in ordered_global.iter().enumerate() {
        local_of_global.push((g, local));
    }
    local_of_global.sort_by_key(|p| p.0);

    let mut local_cons = Vec::with_capacity(comp_cons.len());
    let mut targets = Vec::with_capacity(comp_cons.len());
    for c in comp_cons {
        let mut vars = Vec::with_capacity(c.vars.len());
        for &g in &c.vars {
            if let Ok(idx) = local_of_global.binary_search_by_key(&g, |p| p.0) {
                vars.push(local_of_global[idx].1);
            }
        }
        local_cons.push(vars);
        targets.push(c.target);
    }

    let mut dfs = DfsComponentSolver::new(n, &local_cons, targets);
    dfs.solve(0, 0);
    if dfs.aborted {
        return TankCompResult {
            vars: ordered_global,
            ways_by_k: Vec::new(),
            var_mine_ways: Vec::new(),
            exact: false,
        };
    }
    TankCompResult {
        vars: ordered_global,
        ways_by_k: dfs.ways_by_k,
        var_mine_ways: dfs.var_mine_ways,
        exact: true,
    }
}

fn convolve_log_poly(poly: &[f64], ways_by_k: &[f64], max_total: usize) -> Vec<f64> {
    let mut next_len = (poly.len() + ways_by_k.len()).saturating_sub(1);
    next_len = next_len.min(max_total + 1).max(1);
    let mut out = vec![f64::NEG_INFINITY; next_len];
    for (a, &la) in poly.iter().enumerate() {
        if la == f64::NEG_INFINITY {
            continue;
        }
        for (b, &wb) in ways_by_k.iter().enumerate() {
            if wb <= 0.0 || a + b >= next_len {
                continue;
            }
            let term = la + wb.ln();
            out[a + b] = log_add(out[a + b], term);
        }
    }
    out
}

fn convolve_two_logs(a: &[f64], b: &[f64], max_total: usize) -> Vec<f64> {
    let next_len = (a.len() + b.len())
        .saturating_sub(1)
        .min(max_total + 1)
        .max(1);
    let mut out = vec![f64::NEG_INFINITY; next_len];
    for (i, &la) in a.iter().enumerate() {
        if la == f64::NEG_INFINITY {
            continue;
        }
        for (j, &lb) in b.iter().enumerate() {
            if lb == f64::NEG_INFINITY || i + j >= next_len {
                continue;
            }
            out[i + j] = log_add(out[i + j], la + lb);
        }
    }
    out
}

/// Complete Minesweeper AI analyzer: deterministic propagation, subset subtraction,
/// connected-component Tank CSP, and log-domain binomial global mine recombination.
pub fn analyze_board(board: &MinesBoard) -> AiSnapshot {
    let len = board.len();
    let mut snap = AiSnapshot {
        probs: vec![None; len],
        reasons: vec![String::new(); len],
        ..AiSnapshot::default()
    };

    if matches!(board.status, GameStatus::Won | GameStatus::Lost) {
        for i in 0..len {
            if board.marks[i] == MarkState::Flagged {
                snap.probs[i] = Some(1.0);
                snap.reasons[i] = String::from("已标记地雷");
            }
        }
        snap.summary = if board.status == GameStatus::Won {
            String::from("胜利！所有安全格已清理完毕")
        } else {
            String::from("触发地雷，游戏结束 — 点击笑脸重新开始")
        };
        return snap;
    }

    if !board.mines_placed {
        let base_p = board.total_mines as f64 / len.max(1) as f64;
        for i in 0..len {
            snap.probs[i] = Some(base_p);
            snap.reasons[i] = String::from("首击开局（点击任意格，3×3 区域必安全）");
        }
        let cx = board.cols / 2;
        let cy = board.rows / 2;
        let center = board.idx(cx, cy);
        snap.best_move = Some(AiRecommendation {
            cell: center,
            action: AiActionKind::Reveal,
            mine_prob: 0.0,
            reason: String::from("推荐从中央位置首击开局（确保 3×3 无雷区）"),
        });
        snap.summary = String::from("首击必安全：推荐点击中央开局");
        return snap;
    }

    // Build initial constraints from revealed numbered tiles.
    let mut raw_constraints: Vec<Constraint> = Vec::new();
    let mut inconsistency = false;
    for i in 0..len {
        if board.marks[i] != MarkState::Revealed || board.adj[i] == 0 {
            continue;
        }
        let mut flagged = 0usize;
        let mut hidden = Vec::with_capacity(8);
        for n in board.neighbors(i) {
            match board.marks[n] {
                MarkState::Flagged => flagged += 1,
                MarkState::Hidden | MarkState::Question => hidden.push(n),
                MarkState::Revealed => {}
            }
        }
        let val = board.adj[i] as usize;
        if flagged > val || flagged + hidden.len() < val {
            inconsistency = true;
            continue;
        }
        if !hidden.is_empty() {
            if let Some(c) = Constraint::normalized(hidden, val - flagged) {
                raw_constraints.push(c);
            } else {
                inconsistency = true;
            }
        }
    }

    if inconsistency {
        snap.summary = String::from("警告：当前标雷存在矛盾（标多或漏标），请检查旗帜");
    }

    // Pass 1: Deterministic fixed-point propagation (single-cell + subset subtraction).
    let mut deduced = vec![CellDeduction::Unknown; len];
    let mut active = raw_constraints;
    let mut pass_iters = 0usize;
    loop {
        pass_iters += 1;
        if pass_iters > 64 {
            break;
        }
        let mut changed = false;

        // Strip previously deduced variables from active constraints.
        let mut next_active = Vec::with_capacity(active.len());
        let mut seen_set = BTreeSet::new();
        for mut c in active {
            let mut rem_vars = Vec::with_capacity(c.vars.len());
            let mut rem_target = c.target as isize;
            for v in c.vars {
                match deduced[v] {
                    CellDeduction::Unknown => rem_vars.push(v),
                    CellDeduction::Safe => {}
                    CellDeduction::Mine => rem_target -= 1,
                }
            }
            if rem_target < 0 || rem_target as usize > rem_vars.len() {
                inconsistency = true;
                continue;
            }
            let rt = rem_target as usize;
            if rem_vars.is_empty() {
                continue;
            }
            if rt == 0 {
                for &v in &rem_vars {
                    if deduced[v] == CellDeduction::Unknown {
                        deduced[v] = CellDeduction::Safe;
                        snap.probs[v] = Some(0.0);
                        snap.reasons[v] = String::from("单格计数直推：周围雷数已全部标出");
                        changed = true;
                    }
                }
                continue;
            }
            if rt == rem_vars.len() {
                for &v in &rem_vars {
                    if deduced[v] == CellDeduction::Unknown {
                        deduced[v] = CellDeduction::Mine;
                        snap.probs[v] = Some(1.0);
                        snap.reasons[v] = String::from("单格计数直推：剩余未翻邻格均必为雷");
                        changed = true;
                    }
                }
                continue;
            }
            c.vars = rem_vars;
            c.target = rt;
            if seen_set.insert((c.vars.clone(), c.target)) {
                next_active.push(c);
            }
        }
        active = next_active;

        // Pairwise subset subtraction & pigeonhole overlap deduction.
        let mut extra_constraints: Vec<Constraint> = Vec::new();
        for i in 0..active.len() {
            for j in (i + 1)..active.len() {
                let a = &active[i];
                let b = &active[j];
                let mut only_a = Vec::new();
                let mut only_b = Vec::new();
                let mut inter = 0usize;
                let (mut ia, mut ib) = (0usize, 0usize);
                while ia < a.vars.len() && ib < b.vars.len() {
                    if a.vars[ia] == b.vars[ib] {
                        inter += 1;
                        ia += 1;
                        ib += 1;
                    } else if a.vars[ia] < b.vars[ib] {
                        only_a.push(a.vars[ia]);
                        ia += 1;
                    } else {
                        only_b.push(b.vars[ib]);
                        ib += 1;
                    }
                }
                only_a.extend_from_slice(&a.vars[ia..]);
                only_b.extend_from_slice(&b.vars[ib..]);
                if inter == 0 {
                    continue;
                }

                // True subset relations A ⊂ B or B ⊂ A generate exact difference constraint.
                if only_a.is_empty() && !only_b.is_empty() && b.target >= a.target {
                    let dt = b.target - a.target;
                    if dt == 0 {
                        for &v in &only_b {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Safe;
                                snap.probs[v] = Some(0.0);
                                snap.reasons[v] = String::from("约束子集相减：差集必定全安全");
                                changed = true;
                            }
                        }
                    } else if dt == only_b.len() {
                        for &v in &only_b {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Mine;
                                snap.probs[v] = Some(1.0);
                                snap.reasons[v] = String::from("约束子集相减：差集必定全为雷");
                                changed = true;
                            }
                        }
                    } else if extra_constraints.len() < 128 {
                        if let Some(sub) = Constraint::normalized(only_b, dt) {
                            extra_constraints.push(sub);
                        }
                    }
                } else if only_b.is_empty() && !only_a.is_empty() && a.target >= b.target {
                    let dt = a.target - b.target;
                    if dt == 0 {
                        for &v in &only_a {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Safe;
                                snap.probs[v] = Some(0.0);
                                snap.reasons[v] = String::from("约束子集相减：差集必定全安全");
                                changed = true;
                            }
                        }
                    } else if dt == only_a.len() {
                        for &v in &only_a {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Mine;
                                snap.probs[v] = Some(1.0);
                                snap.reasons[v] = String::from("约束子集相减：差集必定全为雷");
                                changed = true;
                            }
                        }
                    } else if extra_constraints.len() < 128 {
                        if let Some(sub) = Constraint::normalized(only_a, dt) {
                            extra_constraints.push(sub);
                        }
                    }
                } else {
                    // General overlapping pigeonhole deduction:
                    // max_shared = target(A) - min(A) vs min_shared = target(B) - |B_only|
                    // When a.target - b.target == only_a.len(), all of only_a are Mines & only_b are Safe!
                    if a.target >= b.target && a.target - b.target == only_a.len() {
                        for &v in &only_a {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Mine;
                                snap.probs[v] = Some(1.0);
                                snap.reasons[v] = String::from("双约束交差抽屉原理：必雷");
                                changed = true;
                            }
                        }
                        for &v in &only_b {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Safe;
                                snap.probs[v] = Some(0.0);
                                snap.reasons[v] = String::from("双约束交差抽屉原理：必安全");
                                changed = true;
                            }
                        }
                    }
                    if b.target >= a.target && b.target - a.target == only_b.len() {
                        for &v in &only_b {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Mine;
                                snap.probs[v] = Some(1.0);
                                snap.reasons[v] = String::from("双约束交差抽屉原理：必雷");
                                changed = true;
                            }
                        }
                        for &v in &only_a {
                            if deduced[v] == CellDeduction::Unknown {
                                deduced[v] = CellDeduction::Safe;
                                snap.probs[v] = Some(0.0);
                                snap.reasons[v] = String::from("双约束交差抽屉原理：必安全");
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        let prev_len = active.len();
        for c in extra_constraints {
            if seen_set.insert((c.vars.clone(), c.target)) {
                active.push(c);
            }
        }
        if !changed && active.len() == prev_len {
            break;
        }
    }

    // Collect hidden/question frontier vars still unresolved in `active` vs outside cells.
    let mut var_in_frontier = vec![false; len];
    for c in &active {
        for &v in &c.vars {
            var_in_frontier[v] = true;
        }
    }

    let mut frontier_vars = Vec::new();
    let mut outside_cells = Vec::new();
    let mut deduced_mines_count = 0usize;
    for i in 0..len {
        if board.marks[i] == MarkState::Flagged {
            snap.probs[i] = Some(1.0);
            snap.reasons[i] = String::from("已标旗");
        } else if matches!(board.marks[i], MarkState::Hidden | MarkState::Question) {
            match deduced[i] {
                CellDeduction::Safe => snap.safe_cells.push(i),
                CellDeduction::Mine => {
                    snap.mine_cells.push(i);
                    deduced_mines_count += 1;
                }
                CellDeduction::Unknown => {
                    if var_in_frontier[i] {
                        frontier_vars.push(i);
                    } else {
                        outside_cells.push(i);
                    }
                }
            }
        }
    }

    let rem_mines_global = board
        .total_mines
        .saturating_sub(board.flags_placed + deduced_mines_count);

    // Decompose active constraints into connected components over `frontier_vars`.
    let mut comp_id_of_con = vec![None::<usize>; active.len()];
    let mut comp_id_of_var = vec![None::<usize>; len];
    let mut components: Vec<(Vec<usize>, Vec<usize>)> = Vec::new(); // (var global indices, con indices)

    // Pre-build adjacency from var -> constraint indices
    let mut var_cons = vec![Vec::<usize>::new(); len];
    for (ci, c) in active.iter().enumerate() {
        for &v in &c.vars {
            var_cons[v].push(ci);
        }
    }

    for start_v in frontier_vars {
        if comp_id_of_var[start_v].is_some() {
            continue;
        }
        let cid = components.len();
        let mut q = VecDeque::new();
        let mut c_vars = Vec::new();
        let mut c_cons = Vec::new();
        comp_id_of_var[start_v] = Some(cid);
        q.push_back(start_v);
        while let Some(v) = q.pop_front() {
            c_vars.push(v);
            for &ci in &var_cons[v] {
                if comp_id_of_con[ci].is_none() {
                    comp_id_of_con[ci] = Some(cid);
                    c_cons.push(ci);
                    for &nv in &active[ci].vars {
                        if comp_id_of_var[nv].is_none() {
                            comp_id_of_var[nv] = Some(cid);
                            q.push_back(nv);
                        }
                    }
                }
            }
        }
        c_vars.sort_unstable();
        components.push((c_vars, c_cons));
    }

    // Pass 2: Tank CSP backtracking per component + Binomial Global Recombination.
    let mut exact_results: Vec<TankCompResult> = Vec::new();
    let mut large_expected_mines = 0.0f64;
    let mut large_var_count = 0usize;

    for (vars, con_idxs) in &components {
        let cons: Vec<&Constraint> = con_idxs.iter().map(|&ci| &active[ci]).collect();
        let res = solve_component_tank(vars, &cons);
        if res.exact && res.ways_by_k.iter().any(|&w| w > 0.0) {
            exact_results.push(res);
        } else {
            // Large component (>22 vars) or aborted DFS fallback: local constraint average.
            snap.exact_global = false;
            large_var_count += vars.len();
            let mut sum_expected = 0.0f64;
            for &v in vars {
                let mut acc = 0.0f64;
                let mut cnt = 0usize;
                for &ci in &var_cons[v] {
                    let c = &active[ci];
                    if !c.vars.is_empty() {
                        acc += c.target as f64 / c.vars.len() as f64;
                        cnt += 1;
                    }
                }
                let est = if cnt > 0 {
                    (acc / cnt as f64).clamp(0.01, 0.99)
                } else {
                    0.5
                };
                sum_expected += est;
                snap.probs[v] = Some(est);
                snap.reasons[v] = format!(
                    "超大连通分量({}格)降级局部概率估计：约 {:.1}%",
                    vars.len(),
                    est * 100.0
                );
            }
            large_expected_mines += sum_expected;
        }
    }

    // Adjust remaining global mine budget for exact components + outside cells.
    let approx_large_mines = large_expected_mines.round() as usize;
    let exact_plus_outside_budget = rem_mines_global.saturating_sub(approx_large_mines);
    let outside_n = outside_cells.len();
    let binom = LogBinomial::new(len);

    // Prefix & suffix log-convolution across `exact_results` to get `other_log[c][m]`.
    let m_count = exact_results.len();
    if m_count == 0 {
        let outside_p = if outside_n > 0 {
            (exact_plus_outside_budget as f64 / outside_n as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        for &v in &outside_cells {
            snap.probs[v] = Some(outside_p);
            snap.reasons[v] = if outside_p == 0.0 {
                String::from("剩余雷数归零（其余未探索格全安全）")
            } else if (outside_p - 1.0).abs() < 1e-9 {
                String::from("剩余雷数占满未探索区（其余未探索格全为雷）")
            } else {
                format!(
                    "非边界自由格：按剩余 {exact_plus_outside_budget}/{outside_n} 均摊概率 {:.1}%",
                    outside_p * 100.0
                )
            };
            if outside_p < 1e-9 && deduced[v] == CellDeduction::Unknown {
                snap.safe_cells.push(v);
            } else if (outside_p - 1.0).abs() < 1e-9 && deduced[v] == CellDeduction::Unknown {
                snap.mine_cells.push(v);
            }
        }
    } else {
        let mut prefix = Vec::with_capacity(m_count + 1);
        prefix.push(vec![0.0f64]); // 1 way to use 0 mines -> ln(1) = 0
        for comp in &exact_results {
            let next = convolve_log_poly(prefix.last().unwrap(), &comp.ways_by_k, rem_mines_global);
            prefix.push(next);
        }
        let mut suffix = vec![Vec::new(); m_count + 1];
        suffix[m_count] = vec![0.0f64];
        for i in (0..m_count).rev() {
            suffix[i] = convolve_log_poly(
                &suffix[i + 1],
                &exact_results[i].ways_by_k,
                rem_mines_global,
            );
        }

        // Total global log weight Z across all exact components + outside binomial combinations.
        let total_comp_poly = &prefix[m_count];
        let mut total_log_z = f64::NEG_INFINITY;
        let mut outside_mine_weighted_log = f64::NEG_INFINITY;

        for (k_front, &log_w_front) in total_comp_poly.iter().enumerate() {
            if log_w_front == f64::NEG_INFINITY || k_front > exact_plus_outside_budget {
                continue;
            }
            let k_out = exact_plus_outside_budget - k_front;
            if k_out > outside_n {
                continue;
            }
            let log_term = log_w_front + binom.ln_comb(outside_n, k_out);
            total_log_z = log_add(total_log_z, log_term);
            if outside_n > 0 && k_out > 0 {
                let p_out_ratio = (k_out as f64 / outside_n as f64).ln();
                outside_mine_weighted_log =
                    log_add(outside_mine_weighted_log, log_term + p_out_ratio);
            }
        }

        if total_log_z != f64::NEG_INFINITY {
            // Exact marginal probabilities for every cell in each exact component!
            for (ci, comp) in exact_results.iter().enumerate() {
                let other_poly =
                    convolve_two_logs(&prefix[ci], &suffix[ci + 1], exact_plus_outside_budget);
                let mut rest_weight_for_k = vec![f64::NEG_INFINITY; comp.ways_by_k.len()];
                for (k, rw) in rest_weight_for_k.iter_mut().enumerate() {
                    if comp.ways_by_k[k] <= 0.0 || k > exact_plus_outside_budget {
                        continue;
                    }
                    let mut acc = f64::NEG_INFINITY;
                    let rem_after_c = exact_plus_outside_budget - k;
                    for (m_other, &lo) in other_poly.iter().enumerate() {
                        if lo == f64::NEG_INFINITY || m_other > rem_after_c {
                            continue;
                        }
                        let k_out = rem_after_c - m_other;
                        if k_out <= outside_n {
                            acc = log_add(acc, lo + binom.ln_comb(outside_n, k_out));
                        }
                    }
                    *rw = acc;
                }

                for (vi, &global_cell) in comp.vars.iter().enumerate() {
                    let mut num_log = f64::NEG_INFINITY;
                    for (k, &w_mine) in comp.var_mine_ways[vi].iter().enumerate() {
                        if w_mine > 0.0 && rest_weight_for_k[k] != f64::NEG_INFINITY {
                            num_log = log_add(num_log, w_mine.ln() + rest_weight_for_k[k]);
                        }
                    }
                    let p = if num_log == f64::NEG_INFINITY {
                        0.0
                    } else {
                        (num_log - total_log_z).exp().clamp(0.0, 1.0)
                    };
                    let comp_sz = comp.vars.len();
                    if p < 1e-9 {
                        snap.probs[global_cell] = Some(0.0);
                        snap.reasons[global_cell] = format!(
                            "Tank CSP 精确推演({comp_sz}格分量+二项加权)：全球局无雷可能(0%)"
                        );
                        snap.safe_cells.push(global_cell);
                    } else if p > 1.0 - 1e-9 {
                        snap.probs[global_cell] = Some(1.0);
                        snap.reasons[global_cell] =
                            format!("Tank CSP 精确推演({comp_sz}格分量+二项加权)：必为雷(100%)");
                        snap.mine_cells.push(global_cell);
                    } else {
                        snap.probs[global_cell] = Some(p);
                        snap.reasons[global_cell] = format!(
                            "Tank CSP 精确枚举({comp_sz}格分量+二项加权)：踩雷率 {:.1}%",
                            p * 100.0
                        );
                    }
                }
            }

            // Marginal probability for outside unconstrained cells.
            if outside_n > 0 {
                let outside_p = if outside_mine_weighted_log == f64::NEG_INFINITY {
                    0.0
                } else {
                    (outside_mine_weighted_log - total_log_z)
                        .exp()
                        .clamp(0.0, 1.0)
                };
                for &v in &outside_cells {
                    snap.probs[v] = Some(outside_p);
                    if outside_p < 1e-9 {
                        snap.reasons[v] =
                            String::from("全局剩余雷数已被边界全部分配：非边界格必安全(0%)");
                        snap.safe_cells.push(v);
                    } else if outside_p > 1.0 - 1e-9 {
                        snap.reasons[v] =
                            String::from("全局剩余雷数占满非边界区：非边界格必为雷(100%)");
                        snap.mine_cells.push(v);
                    } else {
                        snap.reasons[v] = format!(
                            "非边界格(剩余组合二项式权重)：踩雷率 {:.1}%",
                            outside_p * 100.0
                        );
                    }
                }
            }
        } else {
            // Inconsistent global mine count fallback -> use local component unweighted ratio.
            snap.exact_global = false;
            for comp in &exact_results {
                let total_ways: f64 = comp.ways_by_k.iter().sum();
                for (vi, &g) in comp.vars.iter().enumerate() {
                    let mw: f64 = comp.var_mine_ways[vi].iter().sum();
                    let p = if total_ways > 0.0 {
                        (mw / total_ways).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    snap.probs[g] = Some(p);
                    snap.reasons[g] =
                        format!("独立分量概率（全局旗数可能冲突）：{:.1}%", p * 100.0);
                }
            }
            let out_p = if outside_n > 0 {
                (rem_mines_global as f64 / outside_n as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            for &v in &outside_cells {
                snap.probs[v] = Some(out_p);
                snap.reasons[v] = format!("非边界均摊概率估计：{:.1}%", out_p * 100.0);
            }
        }
    }

    snap.safe_cells.sort_unstable();
    snap.safe_cells.dedup();
    snap.mine_cells.sort_unstable();
    snap.mine_cells.dedup();

    // Pass 3: Select best recommended move (certain safe -> certain mine -> lowest probability
    // with frontier info-gain & corner tie-breaking).
    snap.best_move = choose_best_move(board, &snap, &var_in_frontier);
    if !inconsistency {
        let safe_n = snap.safe_cells.len();
        let mine_n = snap.mine_cells.len();
        snap.summary = if safe_n > 0 || mine_n > 0 {
            format!(
                "AI 就绪：发现 {safe_n} 个必安全格、{mine_n} 个必雷格{}",
                if snap.exact_global {
                    ""
                } else {
                    "（含超大分量估计）"
                }
            )
        } else if let Some(best) = snap.best_move.as_ref() {
            let (bx, by) = board.coords(best.cell);
            format!(
                "需猜测：最优格 ({},{}) 踩雷率 {:.1}%（{}）",
                bx + 1,
                by + 1,
                best.mine_prob * 100.0,
                if snap.exact_global {
                    "精确二项推理"
                } else {
                    "含局部近似"
                }
            )
        } else if large_var_count > 0 {
            format!("分析完成（含 {large_var_count} 个大边界近似格）")
        } else {
            String::from("AI 分析完成")
        };
    }

    snap
}

fn tie_score_for_reveal(board: &MinesBoard, idx: usize, on_frontier: bool) -> i32 {
    let (x, y) = board.coords(idx);
    let nbrs = board.neighbors(idx);
    let hidden_adj = nbrs
        .iter()
        .filter(|&&n| matches!(board.marks[n], MarkState::Hidden | MarkState::Question))
        .count() as i32;
    let revealed_adj = nbrs
        .iter()
        .filter(|&&n| board.marks[n] == MarkState::Revealed)
        .count() as i32;
    let is_corner = (x == 0 || x + 1 == board.cols) && (y == 0 || y + 1 == board.rows);
    let is_edge = x == 0 || x + 1 == board.cols || y == 0 || y + 1 == board.rows;

    // Higher score wins ties: prefer frontier cells with high info gain (adjacent to
    // both revealed clues and multiple hidden cells); for isolated outside ties prefer
    // corners/edges to reduce 50/50 traps cleanly.
    let mut score = if on_frontier {
        100 + revealed_adj * 12 + hidden_adj * 4
    } else {
        hidden_adj * 2
    };
    if is_corner {
        score += 24;
    } else if is_edge {
        score += 10;
    }
    score
}

fn choose_best_move(
    board: &MinesBoard,
    snap: &AiSnapshot,
    var_in_frontier: &[bool],
) -> Option<AiRecommendation> {
    if let Some(&safe_idx) = snap
        .safe_cells
        .iter()
        .max_by_key(|&&idx| tie_score_for_reveal(board, idx, var_in_frontier[idx]))
    {
        return Some(AiRecommendation {
            cell: safe_idx,
            action: AiActionKind::Reveal,
            mine_prob: 0.0,
            reason: snap.reasons[safe_idx].clone(),
        });
    }

    // If deterministic mine flags exist while no direct 0%-safe cell was immediately found
    // without flagging/clearing, recommend flagging one so human / auto step makes progress.
    if let Some(&mine_idx) = snap.mine_cells.first() {
        return Some(AiRecommendation {
            cell: mine_idx,
            action: AiActionKind::Flag,
            mine_prob: 1.0,
            reason: snap.reasons[mine_idx].clone(),
        });
    }

    // Probability guess: pick global minimum P, breaking near-equal ties (~0.1%) by
    // frontier progress & corner heuristic.
    let mut best_idx = None;
    let mut best_p = 2.0f64;
    let mut best_tie = i32::MIN;

    for (i, prob) in snap.probs.iter().enumerate() {
        if !matches!(board.marks[i], MarkState::Hidden | MarkState::Question) {
            continue;
        }
        let p = match prob {
            Some(v) => *v,
            None => continue,
        };
        let tie = tie_score_for_reveal(board, i, var_in_frontier[i]);
        if p + 1e-4 < best_p || ((p - best_p).abs() <= 1e-4 && tie > best_tie) {
            best_p = p;
            best_tie = tie;
            best_idx = Some(i);
        }
    }

    best_idx.map(|cell| AiRecommendation {
        cell,
        action: AiActionKind::Reveal,
        mine_prob: best_p,
        reason: snap.reasons[cell].clone(),
    })
}

fn execute_ai_step(board: &mut MinesBoard, snap: &AiSnapshot) -> Option<AiActionKind> {
    // Auto/single-step: flag all 100%-certain mines if convenient, or execute best reveal!
    if let Some(&mine_idx) = snap.mine_cells.first() {
        if board.set_flag(mine_idx, true) {
            return Some(AiActionKind::Flag);
        }
    }
    let rec = snap.best_move.as_ref()?;
    match rec.action {
        AiActionKind::Flag => {
            if board.set_flag(rec.cell, true) {
                Some(AiActionKind::Flag)
            } else {
                None
            }
        }
        AiActionKind::Reveal => {
            if board.reveal(rec.cell) {
                Some(AiActionKind::Reveal)
            } else {
                None
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UiHit {
    DiffBeginner,
    DiffIntermediate,
    DiffExpert,
    DiffCustom,
    ToggleZeroGuess,
    ToggleHeatmap,
    AiStep,
    AiAuto,
    Smiley,
    CustomWidthDec,
    CustomWidthInc,
    CustomHeightDec,
    CustomHeightInc,
    CustomMinesDec,
    CustomMinesInc,
    CustomApply,
    Cell(usize),
}

struct MinesLayout {
    menu_bar: Rect,
    btn_beg: Rect,
    btn_int: Rect,
    btn_exp: Rect,
    btn_cust: Rect,
    btn_zeroguess: Rect,
    ai_bar: Rect,
    btn_heatmap: Rect,
    btn_step: Rect,
    btn_auto: Rect,
    custom_bar: Option<Rect>,
    custom_btns: [Rect; 7], // w-, w+, h-, h+, m-, m+, apply
    chassis: Rect,
    led_header: Rect,
    mine_led: Rect,
    smiley: Rect,
    timer_led: Rect,
    board_outer: Rect,
    grid_origin: (i32, i32),
    cell_size: i32,
    status_bar: Rect,
}

impl MinesLayout {
    fn preferred_window_size(diff: Difficulty, show_custom: bool) -> (u16, u16) {
        let (cols, rows, _) = diff.dims();
        let cell = diff.preferred_cell_size();
        let board_w = cols as i32 * cell;
        let board_h = rows as i32 * cell;
        let custom_h = if show_custom { 34 } else { 0 };
        let win_w = (board_w + 46).max(480) as u16;
        let win_h = (TITLEBAR_HEIGHT + 28 + 30 + custom_h + 18 + 46 + 12 + board_h + 20 + 26)
            .max(360) as u16;
        (win_w, win_h)
    }

    fn compute(win_w: i32, win_h: i32, cols: usize, rows: usize, show_custom: bool) -> Self {
        let menu_bar = Rect::new(0, TITLEBAR_HEIGHT, win_w, 28);
        let btn_beg = Rect::new(8, TITLEBAR_HEIGHT + 4, 48, 20);
        let btn_int = Rect::new(btn_beg.right() + 4, TITLEBAR_HEIGHT + 4, 48, 20);
        let btn_exp = Rect::new(btn_int.right() + 4, TITLEBAR_HEIGHT + 4, 48, 20);
        let btn_cust = Rect::new(btn_exp.right() + 4, TITLEBAR_HEIGHT + 4, 58, 20);
        let btn_zeroguess = Rect::new(
            (win_w - 116).max(btn_cust.right() + 8),
            TITLEBAR_HEIGHT + 4,
            108,
            20,
        );

        let ai_bar = Rect::new(0, menu_bar.bottom(), win_w, 30);
        let btn_heatmap = Rect::new(8, ai_bar.y + 4, 104, 22);
        let btn_step = Rect::new(btn_heatmap.right() + 6, ai_bar.y + 4, 108, 22);
        let btn_auto = Rect::new(btn_step.right() + 6, ai_bar.y + 4, 106, 22);

        let mut y_cursor = ai_bar.bottom();
        let mut custom_btns = [Rect::new(0, 0, 0, 0); 7];
        let custom_bar = if show_custom {
            let cb = Rect::new(0, y_cursor, win_w, 34);
            let mut cx = 12;
            let by = cb.y + 6;
            // Width (-/+)
            cx += 26;
            custom_btns[0] = Rect::new(cx, by, 20, 22);
            cx += 50;
            custom_btns[1] = Rect::new(cx, by, 20, 22);
            cx += 30;
            // Height (-/+)
            cx += 26;
            custom_btns[2] = Rect::new(cx, by, 20, 22);
            cx += 50;
            custom_btns[3] = Rect::new(cx, by, 20, 22);
            cx += 30;
            // Mines (-/+)
            cx += 26;
            custom_btns[4] = Rect::new(cx, by, 20, 22);
            cx += 54;
            custom_btns[5] = Rect::new(cx, by, 20, 22);
            custom_btns[6] = Rect::new((win_w - 64).max(cx + 28), by, 54, 22);
            y_cursor = cb.bottom();
            Some(cb)
        } else {
            None
        };

        let status_h = 26;
        let status_bar = Rect::new(0, (win_h - status_h).max(y_cursor + 100), win_w, status_h);
        let chassis = Rect::new(
            6,
            y_cursor + 4,
            (win_w - 12).max(160),
            (status_bar.y - (y_cursor + 8)).max(120),
        );

        let led_header = Rect::new(chassis.x + 10, chassis.y + 10, chassis.w - 20, 42);
        let mine_led = Rect::new(led_header.x + 8, led_header.y + 6, 54, 30);
        let timer_led = Rect::new(led_header.right() - 62, led_header.y + 6, 54, 30);
        let smiley = Rect::new(
            led_header.x + (led_header.w - 28) / 2,
            led_header.y + 7,
            28,
            28,
        );

        let avail_w = (chassis.w - 26).max(cols as i32 * 12);
        let avail_h = (chassis.bottom() - 10 - (led_header.bottom() + 8) - 6).max(rows as i32 * 12);
        let cell_size = (avail_w / cols as i32)
            .min(avail_h / rows as i32)
            .clamp(14, 34);
        let board_w = cols as i32 * cell_size;
        let board_h = rows as i32 * cell_size;
        let board_x = chassis.x + (chassis.w - (board_w + 6)) / 2;
        let board_y = led_header.bottom() + 8;
        let board_outer = Rect::new(board_x, board_y, board_w + 6, board_h + 6);
        let grid_origin = (board_outer.x + 3, board_outer.y + 3);

        Self {
            menu_bar,
            btn_beg,
            btn_int,
            btn_exp,
            btn_cust,
            btn_zeroguess,
            ai_bar,
            btn_heatmap,
            btn_step,
            btn_auto,
            custom_bar,
            custom_btns,
            chassis,
            led_header,
            mine_led,
            smiley,
            timer_led,
            board_outer,
            grid_origin,
            cell_size,
            status_bar,
        }
    }

    fn hit_test(&self, x: i32, y: i32, cols: usize, rows: usize) -> Option<UiHit> {
        if self.btn_beg.contains(x, y) {
            return Some(UiHit::DiffBeginner);
        }
        if self.btn_int.contains(x, y) {
            return Some(UiHit::DiffIntermediate);
        }
        if self.btn_exp.contains(x, y) {
            return Some(UiHit::DiffExpert);
        }
        if self.btn_cust.contains(x, y) {
            return Some(UiHit::DiffCustom);
        }
        if self.btn_zeroguess.contains(x, y) {
            return Some(UiHit::ToggleZeroGuess);
        }
        if self.btn_heatmap.contains(x, y) {
            return Some(UiHit::ToggleHeatmap);
        }
        if self.btn_step.contains(x, y) {
            return Some(UiHit::AiStep);
        }
        if self.btn_auto.contains(x, y) {
            return Some(UiHit::AiAuto);
        }
        if self.custom_bar.is_some() {
            let map = [
                UiHit::CustomWidthDec,
                UiHit::CustomWidthInc,
                UiHit::CustomHeightDec,
                UiHit::CustomHeightInc,
                UiHit::CustomMinesDec,
                UiHit::CustomMinesInc,
                UiHit::CustomApply,
            ];
            for (r, hit) in self.custom_btns.iter().zip(map) {
                if r.contains(x, y) {
                    return Some(hit);
                }
            }
        }
        if self.smiley.contains(x, y) {
            return Some(UiHit::Smiley);
        }
        let (gx, gy) = self.grid_origin;
        let bw = cols as i32 * self.cell_size;
        let bh = rows as i32 * self.cell_size;
        if x >= gx && x < gx + bw && y >= gy && y < gy + bh {
            let cx = ((x - gx) / self.cell_size) as usize;
            let cy = ((y - gy) / self.cell_size) as usize;
            if cx < cols && cy < rows {
                return Some(UiHit::Cell(cy * cols + cx));
            }
        }
        None
    }
}

pub fn run() -> Result<()> {
    let mut diff = Difficulty::Beginner;
    let (init_w, init_h) = MinesLayout::preferred_window_size(diff, false);
    let mut app = App::new("扫雷", Icon::Minesweeper, init_w, init_h)?;
    app.min_width = 460;
    app.min_height = 350;

    let (cols, rows, mines) = diff.dims();
    let mut board = MinesBoard::new(cols, rows, mines, false);
    let mut ai = analyze_board(&board);

    let mut show_heatmap = false;
    let mut auto_play = false;
    let mut show_custom = false;
    let mut custom_draft = (16usize, 16usize, 40usize);
    let mut left_down = false;
    let mut right_down = false;
    let mut middle_down = false;
    let mut smiley_pressed = false;
    let mut hover: Option<UiHit> = None;
    let mut hover_cell: Option<usize> = None;
    let mut last_sec = board.elapsed_seconds();
    let mut last_auto = Instant::now();
    let mut ai_dirty = false;

    loop {
        if app.close {
            break;
        }

        if ai_dirty {
            ai = analyze_board(&board);
            ai_dirty = false;
            app.needs_redraw = true;
        }

        if auto_play
            && matches!(board.status, GameStatus::Ready | GameStatus::Playing)
            && last_auto.elapsed() >= Duration::from_millis(AUTO_STEP_MS)
        {
            if let Some(action) = execute_ai_step(&mut board, &ai) {
                match board.status {
                    GameStatus::Won => {
                        auto_play = false;
                        play_sound(&app.config, SystemSound::Notify);
                    }
                    GameStatus::Lost => {
                        auto_play = false;
                        play_sound(&app.config, SystemSound::Alert);
                    }
                    _ => {
                        if action == AiActionKind::Flag {
                            play_sound(&app.config, SystemSound::Click);
                        }
                    }
                }
                ai_dirty = true;
                app.needs_redraw = true;
            } else {
                auto_play = false;
                app.needs_redraw = true;
            }
            last_auto = Instant::now();
        }

        let cur_sec = board.elapsed_seconds();
        if cur_sec != last_sec {
            last_sec = cur_sec;
            app.needs_redraw = true;
        }

        let layout = MinesLayout::compute(
            app.width as i32,
            app.height as i32,
            board.cols,
            board.rows,
            show_custom,
        );

        if app.needs_redraw {
            let pressed_cells =
                collect_pressed_preview(&board, hover_cell, left_down, right_down, middle_down);
            let smiley = if board.status == GameStatus::Won {
                SmileyFace::Won
            } else if board.status == GameStatus::Lost {
                SmileyFace::Lost
            } else if !pressed_cells.is_empty() && !smiley_pressed {
                SmileyFace::Surprised
            } else {
                SmileyFace::Normal
            };
            draw_mines(
                &mut app.canvas,
                &mut app.text,
                &board,
                &ai,
                &layout,
                diff,
                show_heatmap,
                auto_play,
                show_custom,
                custom_draft,
                hover,
                hover_cell,
                &pressed_cells,
                smiley,
                smiley_pressed,
            );
            app.draw_chrome();
            app.present()?;
        }

        let timeout = if auto_play {
            40
        } else if board.status == GameStatus::Playing {
            200
        } else {
            300
        };

        for event in app.pump_with_fd(None, timeout)?.0 {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    let hit = layout.hit_test(x, y, board.cols, board.rows);
                    let cell = match hit {
                        Some(UiHit::Cell(c)) => Some(c),
                        _ => None,
                    };
                    if hit != hover || cell != hover_cell {
                        hover = hit;
                        hover_cell = cell;
                        let pointer = hit.is_some();
                        app.set_content_cursor(if pointer {
                            CursorKind::Pointer
                        } else {
                            CursorKind::Default
                        });
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x, y, button, down, ..
                } => {
                    let hit = layout.hit_test(x, y, board.cols, board.rows);
                    let cell = match hit {
                        Some(UiHit::Cell(c)) => Some(c),
                        _ => None,
                    };
                    hover = hit;
                    hover_cell = cell;

                    if down {
                        match button {
                            1 => {
                                left_down = true;
                                if hit == Some(UiHit::Smiley) {
                                    smiley_pressed = true;
                                }
                            }
                            2 => middle_down = true,
                            3 => {
                                right_down = true;
                                // Classic XP right-click toggles immediately on press when L is not held.
                                if !left_down {
                                    if let Some(c) = cell {
                                        if board.cycle_mark(c) {
                                            ai_dirty = true;
                                            play_sound(&app.config, SystemSound::Click);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        app.needs_redraw = true;
                    } else {
                        let was_chord = (button == 2 && middle_down)
                            || (left_down && right_down && (button == 1 || button == 3));
                        let prev_status = board.status;
                        let mut changed = false;

                        if button == 1 {
                            if smiley_pressed && hit == Some(UiHit::Smiley) {
                                reset_game(&mut board, diff, &mut auto_play);
                                ai_dirty = true;
                                changed = true;
                            } else if was_chord {
                                if let Some(c) = cell {
                                    if board.chord(c) {
                                        changed = true;
                                    }
                                }
                            } else if !right_down {
                                match hit {
                                    Some(UiHit::Cell(c)) => {
                                        if board.reveal(c) {
                                            changed = true;
                                        }
                                    }
                                    Some(UiHit::DiffBeginner) => {
                                        diff = Difficulty::Beginner;
                                        show_custom = false;
                                        resize_for_difficulty(&mut app, diff, show_custom);
                                        reset_game(&mut board, diff, &mut auto_play);
                                        changed = true;
                                    }
                                    Some(UiHit::DiffIntermediate) => {
                                        diff = Difficulty::Intermediate;
                                        show_custom = false;
                                        resize_for_difficulty(&mut app, diff, show_custom);
                                        reset_game(&mut board, diff, &mut auto_play);
                                        changed = true;
                                    }
                                    Some(UiHit::DiffExpert) => {
                                        diff = Difficulty::Expert;
                                        show_custom = false;
                                        resize_for_difficulty(&mut app, diff, show_custom);
                                        reset_game(&mut board, diff, &mut auto_play);
                                        changed = true;
                                    }
                                    Some(UiHit::DiffCustom) => {
                                        show_custom = !show_custom;
                                        if show_custom {
                                            custom_draft = diff.dims();
                                        }
                                        resize_for_difficulty(&mut app, diff, show_custom);
                                        changed = true;
                                    }
                                    Some(UiHit::ToggleZeroGuess) => {
                                        board.zero_guess_mode = !board.zero_guess_mode;
                                        reset_game(&mut board, diff, &mut auto_play);
                                        changed = true;
                                    }
                                    Some(UiHit::ToggleHeatmap) => {
                                        show_heatmap = !show_heatmap;
                                        changed = true;
                                    }
                                    Some(UiHit::AiStep) => {
                                        if execute_ai_step(&mut board, &ai).is_some() {
                                            changed = true;
                                        }
                                    }
                                    Some(UiHit::AiAuto) => {
                                        if matches!(
                                            board.status,
                                            GameStatus::Ready | GameStatus::Playing
                                        ) {
                                            auto_play = !auto_play;
                                            last_auto = Instant::now();
                                            changed = true;
                                        }
                                    }
                                    Some(UiHit::CustomWidthDec) => {
                                        custom_draft.0 = custom_draft.0.saturating_sub(1).max(9);
                                        let max_m =
                                            (custom_draft.0 * custom_draft.1).saturating_sub(1);
                                        custom_draft.2 = custom_draft.2.min(max_m.max(1));
                                        changed = true;
                                    }
                                    Some(UiHit::CustomWidthInc) => {
                                        custom_draft.0 = (custom_draft.0 + 1).min(30);
                                        changed = true;
                                    }
                                    Some(UiHit::CustomHeightDec) => {
                                        custom_draft.1 = custom_draft.1.saturating_sub(1).max(9);
                                        let max_m =
                                            (custom_draft.0 * custom_draft.1).saturating_sub(1);
                                        custom_draft.2 = custom_draft.2.min(max_m.max(1));
                                        changed = true;
                                    }
                                    Some(UiHit::CustomHeightInc) => {
                                        custom_draft.1 = (custom_draft.1 + 1).min(24);
                                        changed = true;
                                    }
                                    Some(UiHit::CustomMinesDec) => {
                                        custom_draft.2 = custom_draft.2.saturating_sub(1).max(1);
                                        changed = true;
                                    }
                                    Some(UiHit::CustomMinesInc) => {
                                        let max_m =
                                            (custom_draft.0 * custom_draft.1).saturating_sub(1);
                                        custom_draft.2 = (custom_draft.2 + 1).min(max_m.max(1));
                                        changed = true;
                                    }
                                    Some(UiHit::CustomApply) => {
                                        diff = Difficulty::Custom(
                                            custom_draft.0,
                                            custom_draft.1,
                                            custom_draft.2,
                                        );
                                        show_custom = false;
                                        resize_for_difficulty(&mut app, diff, show_custom);
                                        reset_game(&mut board, diff, &mut auto_play);
                                        changed = true;
                                    }
                                    _ => {}
                                }
                            }
                            left_down = false;
                            smiley_pressed = false;
                        } else if button == 2 {
                            if was_chord {
                                if let Some(c) = cell {
                                    if board.chord(c) {
                                        changed = true;
                                    }
                                }
                            }
                            middle_down = false;
                        } else if button == 3 {
                            if was_chord {
                                if let Some(c) = cell {
                                    if board.chord(c) {
                                        changed = true;
                                    }
                                }
                            }
                            right_down = false;
                        }

                        if changed {
                            if board.status != prev_status {
                                match board.status {
                                    GameStatus::Won => {
                                        auto_play = false;
                                        play_sound(&app.config, SystemSound::Notify);
                                    }
                                    GameStatus::Lost => {
                                        auto_play = false;
                                        play_sound(&app.config, SystemSound::Alert);
                                    }
                                    _ => play_sound(&app.config, SystemSound::Click),
                                }
                            } else {
                                play_sound(&app.config, SystemSound::Click);
                            }
                            ai_dirty = true;
                            app.needs_redraw = true;
                        }
                    }
                }
                UiEvent::Key(key) => {
                    let mut changed = true;
                    match key.sym {
                        0x6e | 0x4e | 0xffbf => {
                            // N or F2: reset
                            reset_game(&mut board, diff, &mut auto_play);
                            ai_dirty = true;
                        }
                        0x68 | 0x48 | 0x20 => {
                            // H or Space: AI single step
                            if execute_ai_step(&mut board, &ai).is_some() {
                                ai_dirty = true;
                                match board.status {
                                    GameStatus::Won => {
                                        auto_play = false;
                                        play_sound(&app.config, SystemSound::Notify);
                                    }
                                    GameStatus::Lost => {
                                        auto_play = false;
                                        play_sound(&app.config, SystemSound::Alert);
                                    }
                                    _ => play_sound(&app.config, SystemSound::Click),
                                }
                            }
                        }
                        0x70 | 0x50 => show_heatmap = !show_heatmap, // P: probability overlay
                        0x61 | 0x41 => {
                            // A: toggle auto play
                            if matches!(board.status, GameStatus::Ready | GameStatus::Playing) {
                                auto_play = !auto_play;
                                last_auto = Instant::now();
                            }
                        }
                        0x67 | 0x47 => {
                            // G: toggle Zero-Guess mode
                            board.zero_guess_mode = !board.zero_guess_mode;
                            reset_game(&mut board, diff, &mut auto_play);
                            ai_dirty = true;
                        }
                        _ => changed = false,
                    }
                    if changed {
                        app.needs_redraw = true;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn reset_game(board: &mut MinesBoard, diff: Difficulty, auto_play: &mut bool) {
    let (cols, rows, mines) = diff.dims();
    let zg = board.zero_guess_mode;
    *board = MinesBoard::new(cols, rows, mines, zg);
    *auto_play = false;
}

fn resize_for_difficulty(app: &mut App, diff: Difficulty, show_custom: bool) {
    let (w, h) = MinesLayout::preferred_window_size(diff, show_custom);
    let _ = app.x.conn.configure_window(
        app.win,
        &x11rb::protocol::xproto::ConfigureWindowAux::new()
            .width(u32::from(w))
            .height(u32::from(h)),
    );
    let _ = app.x.flush();
}

fn collect_pressed_preview(
    board: &MinesBoard,
    hover_cell: Option<usize>,
    left: bool,
    right: bool,
    middle: bool,
) -> Vec<usize> {
    if matches!(board.status, GameStatus::Won | GameStatus::Lost) {
        return Vec::new();
    }
    let idx = match hover_cell {
        Some(c) => c,
        None => return Vec::new(),
    };
    if middle || (left && right) {
        let mut out = Vec::new();
        if matches!(board.marks[idx], MarkState::Hidden | MarkState::Question) {
            out.push(idx);
        }
        for n in board.neighbors(idx) {
            if matches!(board.marks[n], MarkState::Hidden | MarkState::Question) {
                out.push(n);
            }
        }
        out
    } else if left && matches!(board.marks[idx], MarkState::Hidden | MarkState::Question) {
        vec![idx]
    } else {
        Vec::new()
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_mines(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    board: &MinesBoard,
    ai: &AiSnapshot,
    layout: &MinesLayout,
    diff: Difficulty,
    show_heatmap: bool,
    auto_play: bool,
    show_custom: bool,
    custom_draft: (usize, usize, usize),
    hover: Option<UiHit>,
    hover_cell: Option<usize>,
    pressed_cells: &[usize],
    smiley: SmileyFace,
    smiley_pressed: bool,
) {
    // Classic XP outer client fill
    canvas.fill(
        Rect::new(
            0,
            TITLEBAR_HEIGHT,
            canvas.width as i32,
            canvas.height as i32,
        ),
        XP_FACE,
    );

    // Menu & AI toolbar strips
    draw_toolbar_strip(
        canvas,
        text,
        board,
        layout,
        diff,
        show_heatmap,
        auto_play,
        show_custom,
        hover,
    );
    if let Some(cb) = layout.custom_bar {
        draw_custom_bar(canvas, text, cb, &layout.custom_btns, custom_draft, hover);
    }

    // Classic XP 3D #C0C0C0 chassis
    draw_raised_bevel(canvas, layout.chassis, 3);
    draw_sunken_bevel(canvas, layout.led_header, 2);

    draw_seven_seg(canvas, layout.mine_led, board.remaining_mines_display());
    draw_seven_seg(canvas, layout.timer_led, board.elapsed_seconds() as i32);
    draw_smiley_button(canvas, layout.smiley, smiley, smiley_pressed);

    // Recessed board frame & cells
    draw_sunken_bevel(canvas, layout.board_outer, 3);
    let (gx, gy) = layout.grid_origin;
    let cs = layout.cell_size;
    let rec_cell = ai.best_move.as_ref().map(|m| m.cell);

    for row in 0..board.rows {
        for col in 0..board.cols {
            let idx = board.idx(col, row);
            let r = Rect::new(gx + col as i32 * cs, gy + row as i32 * cs, cs, cs);
            let is_pressed = pressed_cells.contains(&idx);
            draw_cell(
                canvas,
                text,
                board,
                ai,
                idx,
                r,
                is_pressed,
                show_heatmap,
                rec_cell == Some(idx),
                hover_cell == Some(idx),
            );
        }
    }

    // Recessed XP status bar at bottom
    canvas.fill(layout.status_bar, XP_STATUS_BG);
    canvas.fill(
        Rect::new(
            layout.status_bar.x,
            layout.status_bar.y,
            layout.status_bar.w,
            1,
        ),
        XP_SHADOW,
    );
    let status_text = if let Some(hc) = hover_cell {
        let (hx, hy) = board.coords(hc);
        if matches!(board.marks[hc], MarkState::Hidden | MarkState::Question) {
            if let Some(p) = ai.probs[hc] {
                format!(
                    "格({},{}) 踩雷率 {:.1}% — {}",
                    hx + 1,
                    hy + 1,
                    p * 100.0,
                    ai.reasons[hc]
                )
            } else {
                format!("格({},{}) — {}", hx + 1, hy + 1, ai.summary)
            }
        } else if board.marks[hc] == MarkState::Flagged {
            format!("格({},{}) 已标旗 — {}", hx + 1, hy + 1, ai.reasons[hc])
        } else {
            ai.summary.clone()
        }
    } else {
        ai.summary.clone()
    };
    let clipped = text.truncate(
        &status_text,
        (layout.status_bar.w - 16).max(60) as f32,
        11.5,
        Face::Ui,
    );
    text.draw(
        canvas,
        &clipped,
        layout.status_bar.x as f32 + 8.0,
        layout.status_bar.y as f32 + 17.0,
        11.5,
        XP_BLACK,
        Face::Ui,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_toolbar_strip(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    board: &MinesBoard,
    layout: &MinesLayout,
    diff: Difficulty,
    show_heatmap: bool,
    auto_play: bool,
    show_custom: bool,
    hover: Option<UiHit>,
) {
    canvas.fill(layout.menu_bar, XP_TOOLBAR_BG);
    canvas.fill(layout.ai_bar, XP_TOOLBAR_BG);
    canvas.fill(
        Rect::new(0, layout.menu_bar.bottom() - 1, layout.menu_bar.w, 1),
        XP_SHADOW,
    );
    canvas.fill(
        Rect::new(0, layout.ai_bar.bottom() - 1, layout.ai_bar.w, 1),
        XP_SHADOW,
    );

    draw_xp_tab(
        canvas,
        text,
        layout.btn_beg,
        "初级",
        diff == Difficulty::Beginner,
        hover == Some(UiHit::DiffBeginner),
    );
    draw_xp_tab(
        canvas,
        text,
        layout.btn_int,
        "中级",
        diff == Difficulty::Intermediate,
        hover == Some(UiHit::DiffIntermediate),
    );
    draw_xp_tab(
        canvas,
        text,
        layout.btn_exp,
        "高级",
        diff == Difficulty::Expert,
        hover == Some(UiHit::DiffExpert),
    );
    draw_xp_tab(
        canvas,
        text,
        layout.btn_cust,
        "自定义",
        matches!(diff, Difficulty::Custom(..)) || show_custom,
        hover == Some(UiHit::DiffCustom),
    );
    draw_xp_tab(
        canvas,
        text,
        layout.btn_zeroguess,
        if board.zero_guess_mode {
            "✓ 无猜模式"
        } else {
            "经典随机(G)"
        },
        board.zero_guess_mode,
        hover == Some(UiHit::ToggleZeroGuess),
    );

    draw_xp_tab(
        canvas,
        text,
        layout.btn_heatmap,
        if show_heatmap {
            "概率热图: 开"
        } else {
            "概率热图(P)"
        },
        show_heatmap,
        hover == Some(UiHit::ToggleHeatmap),
    );
    draw_xp_tab(
        canvas,
        text,
        layout.btn_step,
        "AI 走一步(H)",
        false,
        hover == Some(UiHit::AiStep),
    );
    draw_xp_tab(
        canvas,
        text,
        layout.btn_auto,
        if auto_play {
            "■ 停止托管"
        } else {
            "AI 自动托管(A)"
        },
        auto_play,
        hover == Some(UiHit::AiAuto),
    );
}

fn draw_custom_bar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    bar: Rect,
    btns: &[Rect; 7],
    draft: (usize, usize, usize),
    hover: Option<UiHit>,
) {
    canvas.fill(bar, XP_TOOLBAR_BG);
    canvas.fill(Rect::new(bar.x, bar.bottom() - 1, bar.w, 1), XP_SHADOW);
    let y_base = bar.y as f32 + 21.0;

    text.draw(canvas, "宽", 10.0, y_base, 11.5, XP_BLACK, Face::Ui);
    draw_xp_tab(
        canvas,
        text,
        btns[0],
        "-",
        false,
        hover == Some(UiHit::CustomWidthDec),
    );
    text.draw_centered(
        canvas,
        &draft.0.to_string(),
        (btns[0].right() + btns[1].x) as f32 * 0.5,
        y_base,
        11.5,
        XP_BLACK,
        Face::Mono,
    );
    draw_xp_tab(
        canvas,
        text,
        btns[1],
        "+",
        false,
        hover == Some(UiHit::CustomWidthInc),
    );

    text.draw(
        canvas,
        "高",
        (btns[1].right() + 8) as f32,
        y_base,
        11.5,
        XP_BLACK,
        Face::Ui,
    );
    draw_xp_tab(
        canvas,
        text,
        btns[2],
        "-",
        false,
        hover == Some(UiHit::CustomHeightDec),
    );
    text.draw_centered(
        canvas,
        &draft.1.to_string(),
        (btns[2].right() + btns[3].x) as f32 * 0.5,
        y_base,
        11.5,
        XP_BLACK,
        Face::Mono,
    );
    draw_xp_tab(
        canvas,
        text,
        btns[3],
        "+",
        false,
        hover == Some(UiHit::CustomHeightInc),
    );

    text.draw(
        canvas,
        "雷",
        (btns[3].right() + 8) as f32,
        y_base,
        11.5,
        XP_BLACK,
        Face::Ui,
    );
    draw_xp_tab(
        canvas,
        text,
        btns[4],
        "-",
        false,
        hover == Some(UiHit::CustomMinesDec),
    );
    text.draw_centered(
        canvas,
        &draft.2.to_string(),
        (btns[4].right() + btns[5].x) as f32 * 0.5,
        y_base,
        11.5,
        XP_BLACK,
        Face::Mono,
    );
    draw_xp_tab(
        canvas,
        text,
        btns[5],
        "+",
        false,
        hover == Some(UiHit::CustomMinesInc),
    );

    draw_xp_tab(
        canvas,
        text,
        btns[6],
        "应用",
        true,
        hover == Some(UiHit::CustomApply),
    );
}

fn draw_xp_tab(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    rect: Rect,
    label: &str,
    active: bool,
    hovered: bool,
) {
    if active {
        canvas.fill(rect, XP_TOOLBAR_ACTIVE);
        canvas.border_1px(rect, XP_DARK);
        text.draw_centered(
            canvas,
            label,
            rect.x as f32 + rect.w as f32 * 0.5,
            rect.y as f32 + rect.h as f32 * 0.5 + 4.0,
            11.0,
            XP_LIGHT,
            Face::Bold,
        );
    } else {
        canvas.fill(rect, if hovered { 0xe5e1d8 } else { XP_FACE });
        draw_raised_bevel(canvas, rect, 1);
        text.draw_centered(
            canvas,
            label,
            rect.x as f32 + rect.w as f32 * 0.5,
            rect.y as f32 + rect.h as f32 * 0.5 + 4.0,
            11.0,
            XP_BLACK,
            Face::Ui,
        );
    }
}

fn draw_raised_bevel(canvas: &mut Canvas, rect: Rect, thickness: i32) {
    for t in 0..thickness {
        let r = Rect::new(rect.x + t, rect.y + t, rect.w - t * 2, rect.h - t * 2);
        if r.w < 2 || r.h < 2 {
            break;
        }
        canvas.fill(Rect::new(r.x, r.y, r.w, 1), XP_LIGHT);
        canvas.fill(Rect::new(r.x, r.y, 1, r.h), XP_LIGHT);
        canvas.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), XP_SHADOW);
        canvas.fill(Rect::new(r.right() - 1, r.y, 1, r.h), XP_SHADOW);
    }
}

fn draw_sunken_bevel(canvas: &mut Canvas, rect: Rect, thickness: i32) {
    for t in 0..thickness {
        let r = Rect::new(rect.x + t, rect.y + t, rect.w - t * 2, rect.h - t * 2);
        if r.w < 2 || r.h < 2 {
            break;
        }
        canvas.fill(Rect::new(r.x, r.y, r.w, 1), XP_SHADOW);
        canvas.fill(Rect::new(r.x, r.y, 1, r.h), XP_SHADOW);
        canvas.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), XP_LIGHT);
        canvas.fill(Rect::new(r.right() - 1, r.y, 1, r.h), XP_LIGHT);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_cell(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    board: &MinesBoard,
    ai: &AiSnapshot,
    idx: usize,
    r: Rect,
    is_pressed: bool,
    show_heatmap: bool,
    is_best: bool,
    is_hover: bool,
) {
    let cs = r.w;
    let lost = board.status == GameStatus::Lost;

    // On Loss: reveal unflagged mines and highlight wrongly flagged cells.
    if lost && board.mines[idx] && board.marks[idx] != MarkState::Flagged {
        let bg = if board.exploded_cell == Some(idx) {
            XP_RED
        } else {
            XP_FACE
        };
        canvas.fill(r, bg);
        canvas.fill(Rect::new(r.x, r.y, cs, 1), XP_SHADOW);
        canvas.fill(Rect::new(r.x, r.y, 1, cs), XP_SHADOW);
        draw_mine_sprite(canvas, r);
        return;
    }

    if lost && !board.mines[idx] && board.marks[idx] == MarkState::Flagged {
        canvas.fill(r, XP_FACE);
        canvas.fill(Rect::new(r.x, r.y, cs, 1), XP_SHADOW);
        canvas.fill(Rect::new(r.x, r.y, 1, cs), XP_SHADOW);
        draw_mine_sprite(canvas, r);
        // Red X over wrong flag
        canvas.line(r.x + 3, r.y + 3, r.right() - 4, r.bottom() - 4, XP_RED, 1.0);
        canvas.line(r.x + 4, r.y + 3, r.right() - 3, r.bottom() - 4, XP_RED, 1.0);
        canvas.line(r.right() - 4, r.y + 3, r.x + 3, r.bottom() - 4, XP_RED, 1.0);
        canvas.line(r.right() - 3, r.y + 3, r.x + 4, r.bottom() - 4, XP_RED, 1.0);
        return;
    }

    match board.marks[idx] {
        MarkState::Revealed => {
            canvas.fill(r, XP_FACE);
            canvas.fill(Rect::new(r.x, r.y, cs, 1), XP_SHADOW);
            canvas.fill(Rect::new(r.x, r.y, 1, cs), XP_SHADOW);
            if board.mines[idx] {
                canvas.fill(r, XP_RED);
                draw_mine_sprite(canvas, r);
            } else {
                let n = board.adj[idx] as usize;
                if n > 0 {
                    let s = n.to_string();
                    let px = (cs as f32 * 0.68).clamp(11.0, 20.0);
                    text.draw_centered(
                        canvas,
                        &s,
                        r.x as f32 + cs as f32 * 0.5,
                        r.y as f32 + cs as f32 * 0.76,
                        px,
                        NUMBER_COLORS[n],
                        Face::Bold,
                    );
                }
            }
        }
        MarkState::Hidden | MarkState::Question => {
            if is_pressed {
                canvas.fill(r, XP_FACE);
                canvas.fill(Rect::new(r.x, r.y, cs, 1), XP_SHADOW);
                canvas.fill(Rect::new(r.x, r.y, 1, cs), XP_SHADOW);
            } else {
                canvas.fill(r, XP_FACE);
                if show_heatmap {
                    if let Some(p) = ai.probs[idx] {
                        let (tint, alpha) = if p < 1e-6 {
                            (0x22c55e, 0.36)
                        } else if p > 1.0 - 1e-6 {
                            (0xef4444, 0.42)
                        } else if p < 0.25 {
                            (0x38bdf8, 0.22)
                        } else if p < 0.50 {
                            (0xf59e0b, 0.28)
                        } else {
                            (0xef4444, 0.32)
                        };
                        canvas.fill_alpha(r, tint, alpha);
                    }
                } else if is_hover {
                    canvas.fill_alpha(r, 0xffffff, 0.14);
                }
                draw_raised_bevel(canvas, r, 2);
            }

            if show_heatmap && is_best {
                canvas.border_1px(Rect::new(r.x + 2, r.y + 2, cs - 4, cs - 4), 0x059669);
            }

            if board.marks[idx] == MarkState::Question {
                let px = (cs as f32 * 0.62).clamp(10.0, 17.0);
                text.draw_centered(
                    canvas,
                    "?",
                    r.x as f32 + cs as f32 * 0.5,
                    r.y as f32 + cs as f32 * 0.74,
                    px,
                    XP_BLACK,
                    Face::Bold,
                );
            } else if show_heatmap {
                if let Some(p) = ai.probs[idx] {
                    let badge = if p < 1e-6 {
                        String::from("✓")
                    } else if p > 1.0 - 1e-6 {
                        String::from("✕")
                    } else {
                        format!("{}", (p * 100.0).round() as i32)
                    };
                    let fg = if p < 1e-6 {
                        0x065f46
                    } else if p > 1.0 - 1e-6 {
                        0x7f1d1d
                    } else {
                        0x111827
                    };
                    let px = (cs as f32 * 0.42).clamp(8.5, 11.5);
                    text.draw_centered(
                        canvas,
                        &badge,
                        r.x as f32 + cs as f32 * 0.5,
                        r.y as f32 + cs as f32 * 0.66,
                        px,
                        fg,
                        Face::Bold,
                    );
                }
            }
        }
        MarkState::Flagged => {
            canvas.fill(r, XP_FACE);
            if show_heatmap && is_best {
                canvas.fill_alpha(r, 0xef4444, 0.25);
            }
            draw_raised_bevel(canvas, r, 2);
            draw_flag_sprite(canvas, r);
        }
    }
}

fn draw_flag_sprite(canvas: &mut Canvas, r: Rect) {
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    let s = (r.w / 4).max(3);
    // Pole
    canvas.fill(Rect::new(cx, cy - s - 1, 2, s * 2 + 1), XP_BLACK);
    // Stepped base
    canvas.fill(Rect::new(cx - s + 1, cy + s - 1, s * 2 - 1, 2), XP_BLACK);
    canvas.fill(Rect::new(cx - s, cy + s + 1, s * 2 + 1, 2), XP_BLACK);
    // Classic XP red triangular pennant
    for dy in 0..=s {
        let w = (s - dy / 2).max(1) + 1;
        canvas.fill(Rect::new(cx + 1 - w, cy - s - 1 + dy, w, 1), XP_RED);
    }
}

fn draw_mine_sprite(canvas: &mut Canvas, r: Rect) {
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    let rad = ((r.w as f32) * 0.27).max(4.0);
    let arm = (r.w as f32 * 0.37).round() as i32;
    let d_arm = (arm as f32 * 0.707).round() as i32;
    canvas.fill(Rect::new(cx - arm, cy, arm * 2 + 1, 2), XP_BLACK);
    canvas.fill(Rect::new(cx, cy - arm, 2, arm * 2 + 1), XP_BLACK);
    canvas.line(
        cx - d_arm,
        cy - d_arm,
        cx + d_arm,
        cy + d_arm,
        XP_BLACK,
        1.0,
    );
    canvas.line(
        cx - d_arm,
        cy + d_arm,
        cx + d_arm,
        cy - d_arm,
        XP_BLACK,
        1.0,
    );
    canvas.circle(cx as f32 + 0.5, cy as f32 + 0.5, rad, XP_BLACK, 1.0);
    // Classic top-left white specular square highlight
    let h_sz = (r.w / 9).max(2);
    canvas.fill(Rect::new(cx - h_sz, cy - h_sz, h_sz, h_sz), XP_LIGHT);
}

fn draw_seven_seg(canvas: &mut Canvas, rect: Rect, value: i32) {
    canvas.fill(rect, XP_BLACK);
    draw_sunken_bevel(canvas, rect, 1);
    let clamped = value.clamp(-99, 999);
    let glyphs: [usize; 3] = if clamped < 0 {
        let abs = (-clamped) as usize;
        [10, (abs / 10) % 10, abs % 10] // 10 = minus sign
    } else {
        let v = clamped as usize;
        [(v / 100) % 10, (v / 10) % 10, v % 10]
    };
    let slot_w = (rect.w - 6) / 3;
    let dh = rect.h - 6;
    for (i, &digit) in glyphs.iter().enumerate() {
        let dx = rect.x + 4 + i as i32 * slot_w;
        let dy = rect.y + 3;
        draw_7seg_digit(canvas, dx, dy, slot_w - 3, dh, digit);
    }
}

fn draw_7seg_digit(canvas: &mut Canvas, x: i32, y: i32, w: i32, h: i32, digit: usize) {
    // Bitmask order: bit0=A(top), 1=B(tr), 2=C(br), 3=D(bot), 4=E(bl), 5=F(tl), 6=G(mid)
    const MASKS: [u8; 11] = [
        0b011_1111, // 0
        0b000_0110, // 1
        0b101_1011, // 2
        0b100_1111, // 3
        0b110_0110, // 4
        0b110_1101, // 5
        0b111_1101, // 6
        0b000_0111, // 7
        0b111_1111, // 8
        0b110_1111, // 9
        0b100_0000, // '-'
    ];
    let mask = MASKS[digit.min(10)];
    let seg_t = 3;
    let mid_y = y + h / 2 - seg_t / 2;
    let segs = [
        Rect::new(x + 2, y, w - 4, seg_t),                     // A
        Rect::new(x + w - seg_t, y + 2, seg_t, h / 2 - 3),     // B
        Rect::new(x + w - seg_t, mid_y + 2, seg_t, h / 2 - 3), // C
        Rect::new(x + 2, y + h - seg_t, w - 4, seg_t),         // D
        Rect::new(x, mid_y + 2, seg_t, h / 2 - 3),             // E
        Rect::new(x, y + 2, seg_t, h / 2 - 3),                 // F
        Rect::new(x + 2, mid_y, w - 4, seg_t),                 // G
    ];
    for (bit, seg) in segs.iter().enumerate() {
        let on = (mask & (1 << bit)) != 0;
        canvas.fill(*seg, if on { XP_LED_ON } else { XP_LED_OFF });
    }
}

fn draw_smiley_button(canvas: &mut Canvas, rect: Rect, face: SmileyFace, pressed: bool) {
    canvas.fill(rect, XP_FACE);
    if pressed {
        draw_sunken_bevel(canvas, rect, 2);
    } else {
        draw_raised_bevel(canvas, rect, 2);
    }
    let off = if pressed { 1 } else { 0 };
    let cx = rect.x as f32 + rect.w as f32 * 0.5 + off as f32;
    let cy = rect.y as f32 + rect.h as f32 * 0.5 + off as f32;
    canvas.circle(cx, cy, 9.5, XP_BLACK, 1.0);
    canvas.circle(cx, cy, 8.2, XP_YELLOW, 1.0);

    let ix = cx.round() as i32;
    let iy = cy.round() as i32;
    match face {
        SmileyFace::Normal => {
            canvas.fill(Rect::new(ix - 4, iy - 4, 2, 2), XP_BLACK);
            canvas.fill(Rect::new(ix + 2, iy - 4, 2, 2), XP_BLACK);
            canvas.fill(Rect::new(ix - 3, iy + 2, 7, 1), XP_BLACK);
            canvas.fill(Rect::new(ix - 4, iy + 1, 1, 1), XP_BLACK);
            canvas.fill(Rect::new(ix + 4, iy + 1, 1, 1), XP_BLACK);
        }
        SmileyFace::Surprised => {
            canvas.fill(Rect::new(ix - 4, iy - 4, 2, 2), XP_BLACK);
            canvas.fill(Rect::new(ix + 2, iy - 4, 2, 2), XP_BLACK);
            canvas.ring(cx, cy + 3.0, 2.2, 1.3, XP_BLACK, 1.0);
        }
        SmileyFace::Won => {
            // Classic XP cool sunglasses
            canvas.fill(Rect::new(ix - 6, iy - 4, 5, 3), XP_BLACK);
            canvas.fill(Rect::new(ix + 1, iy - 4, 5, 3), XP_BLACK);
            canvas.fill(Rect::new(ix - 1, iy - 3, 2, 1), XP_BLACK);
            canvas.fill(Rect::new(ix - 3, iy + 2, 7, 1), XP_BLACK);
            canvas.fill(Rect::new(ix - 4, iy + 1, 1, 1), XP_BLACK);
            canvas.fill(Rect::new(ix + 4, iy + 1, 1, 1), XP_BLACK);
        }
        SmileyFace::Lost => {
            // X eyes + frown
            canvas.line(ix - 5, iy - 5, ix - 2, iy - 2, XP_BLACK, 1.0);
            canvas.line(ix - 2, iy - 5, ix - 5, iy - 2, XP_BLACK, 1.0);
            canvas.line(ix + 2, iy - 5, ix + 5, iy - 2, XP_BLACK, 1.0);
            canvas.line(ix + 5, iy - 5, ix + 2, iy - 2, XP_BLACK, 1.0);
            canvas.fill(Rect::new(ix - 3, iy + 2, 7, 1), XP_BLACK);
            canvas.fill(Rect::new(ix - 4, iy + 3, 1, 1), XP_BLACK);
            canvas.fill(Rect::new(ix + 4, iy + 3, 1, 1), XP_BLACK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_click_3x3_safe_and_flood_opens() {
        let mut board = MinesBoard::new(9, 9, 10, false);
        assert!(board.reveal(40)); // center (4,4)
        assert_eq!(board.status, GameStatus::Playing);
        assert!(board.mines_placed);
        // Entire 3x3 around center must be safe & revealed!
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let idx = board.idx((4 + dx) as usize, (4 + dy) as usize);
                assert!(!board.mines[idx]);
                assert_eq!(board.marks[idx], MarkState::Revealed);
            }
        }
    }

    #[test]
    fn right_click_cycles_and_chord_reveals() {
        // 3x3 board with single mine at (0, 0) = idx 0
        let mut board = MinesBoard::from_preset(3, 3, &[0]);
        assert!(board.reveal(4)); // (1, 1) shows '1'
        assert_eq!(board.marks[4], MarkState::Revealed);
        assert_eq!(board.adj[4], 1);

        // Flag -> Question -> Hidden -> Flag
        assert!(board.cycle_mark(0));
        assert_eq!(board.marks[0], MarkState::Flagged);
        assert_eq!(board.remaining_mines_display(), 0);
        assert!(board.cycle_mark(0));
        assert_eq!(board.marks[0], MarkState::Question);
        assert_eq!(board.remaining_mines_display(), 1);
        assert!(board.cycle_mark(0));
        assert_eq!(board.marks[0], MarkState::Hidden);

        assert!(board.cycle_mark(0)); // back to Flagged at 0
        assert!(board.chord(4));
        assert_eq!(board.status, GameStatus::Won);
    }

    #[test]
    fn ai_deterministic_solves_classic_1_2_1_pattern() {
        // 3x3 board: row 0 revealed as [1, 2, 1], mines must be at (0,1)=3 and (2,1)=5,
        // while (1,1)=4 and bottom row are safe! Wait: place mines at 3 and 5, reveal 0,1,2.
        // Actually in 3x3 with row1=[M, _, M] and row2=[0,0,0]: row0=[1, 2, 1]!
        let mut board = MinesBoard::from_preset(3, 3, &[3, 5]);
        board.marks[0] = MarkState::Revealed;
        board.marks[1] = MarkState::Revealed;
        board.marks[2] = MarkState::Revealed;
        board.revealed_safe = 3;

        let snap = analyze_board(&board);
        assert!(
            snap.safe_cells.contains(&4),
            "middle cell must be 0% safe: {:?}",
            snap
        );
        assert!(snap.mine_cells.contains(&3) && snap.mine_cells.contains(&5));
        assert_eq!(snap.probs[4], Some(0.0));
        assert_eq!(snap.probs[3], Some(1.0));
        assert_eq!(snap.probs[5], Some(1.0));
    }

    #[test]
    fn ai_tank_csp_and_binomial_weighting_computes_exact_marginals() {
        // 5x1 board: cell 0 revealed '1', cell 1 & 2 adjacent? Actually in 4x1:
        // cell 0 = revealed '1' (neighbors: cell 1). Wait let's use a 4x2 board:
        // Reveal (0,0)=1, (1,0)=1 where mines can be either {(2,0)} (uses 1 mine, sharing between both?)
        // Setup a real ambiguity that needs binomial global budget!
        // 4x1 board: cells [R0=1, H1, H2, H3], mines could be 1 or 2.
        // Actually cell 0 revealed '1' touches only H1! Let's make 4x2:
        // Row 0: R(0)=1  R(1)=1  H(2)  H(3)
        // Row 1: R(4)=1  H(5)    H(6)  H(7)
        // If (0)=1 touches {1(R),4(R),5(H)} -> wait H(5) would be 100%!
        // Let's make classic two-var frontier + outside cells:
        // 5x2: row 0 all revealed except (4,0):
        // Reveal (0,0)=0, (1,0)=1, (2,0)=0?
        // Simplest symmetric 2-var component:
        // 3x2 board: (0,0) = revealed '1', (1,0) = revealed '1', wait:
        // On 4x2: revealed (0,0)=1 touches H(1,0) and H(0,1). H(1,1) also revealed? No:
        // Reveal ONLY (0,0)=1 in a 4x2 board with 2 total mines!
        // Wait: (0,0) touches (1,0), (0,1), (1,1) [3 vars, target 1]. Suppose (1,1) is revealed '0'? Impossible.
        // Suppose (0,0) is revealed '1' and total_mines = 1 on a 3x2 board (6 cells: 3 frontier vars + 2 outside vars):
        // Frontier {1,3,4} takes EXACTLY 1 mine in 3 ways; outside {2,5} MUST take 0 mines!!
        // So binomial weighting gives outside cells P=0.0% SAFE, frontier vars P=1/3!
        let mut board = MinesBoard::from_preset(3, 2, &[1]); // total_mines = 1
        board.marks[0] = MarkState::Revealed;
        board.revealed_safe = 1;

        let snap = analyze_board(&board);
        assert!(snap.exact_global);
        // Outside cells (2, 5) MUST be 0% safe because the sole mine must be in {1, 3, 4}!
        assert_eq!(snap.probs[2], Some(0.0));
        assert_eq!(snap.probs[5], Some(0.0));
        assert!(snap.safe_cells.contains(&2));
        assert!(snap.safe_cells.contains(&5));
        for v in [1usize, 3, 4] {
            let p = snap.probs[v].unwrap();
            assert!((p - (1.0 / 3.0)).abs() < 1e-6, "var {v} prob={p}");
        }
    }

    #[test]
    fn ai_binomial_multi_component_endgame_weights_correctly() {
        // Two independent 2-cell components, each needing 1 mine (`1-1` pairs):
        // 7x1 board:
        // 0: R('1') -> touches {1} ? Wait 1D can't have 2-var unshared, use 6x2!
        // Cols 0..6, 2 rows:
        // Component A at col 0..2: reveal (0,0)=1, reveal (0,1)=1 -> Wait let's use:
        // On 6x2 (12 cells), 2 total mines:
        // Cell (0,0) revealed '1', (0,1) revealed? If (0,0)=1 & (1,0) covered, (0,1) covered -> { (1,0), (0,1), (1,1) }
        // Let's construct non-uniform ways: Comp A = 3 vars taking 1 mine (3 ways),
        // Wait: even better, Comp A = cell (0,0)=1 -> {1, 6, 7} [k=1 in 3 ways];
        // Comp B = cell (5,0)=1 -> {4, 10, 11} [k=1 in 3 ways]; total_mines = 3;
        // Reveal (0,0) and (5,0) (with mines at 1, 10, and outside 2).
        // Outside remaining hidden: {(2,0)=2, (3,0)=3, (2,1)=8, (3,1)=9} = 4 cells!
        // Since Comp A takes 1 mine and Comp B takes 1 mine, remaining mine in outside is 3-2 = 1/4 = 0.25!
        let mut board = MinesBoard::from_preset(6, 2, &[1, 10, 2]);
        board.marks[0] = MarkState::Revealed;
        board.marks[5] = MarkState::Revealed;
        board.revealed_safe = 2;

        let snap = analyze_board(&board);
        assert!(snap.exact_global);
        for out_cell in [2usize, 3, 8, 9] {
            let p = snap.probs[out_cell].unwrap();
            assert!((p - 0.25).abs() < 1e-6, "outside cell {out_cell} prob={p}");
        }
    }

    #[test]
    fn zero_guess_generator_produces_solvable_beginner_board() {
        let mut board = MinesBoard::new(9, 9, 10, true);
        assert!(board.reveal(40));
        // Play exclusively using AI steps with zero guessing!
        for _ in 0..100 {
            if board.status == GameStatus::Won {
                break;
            }
            assert_ne!(board.status, GameStatus::Lost);
            let snap = analyze_board(&board);
            assert!(
                !snap.safe_cells.is_empty(),
                "zero-guess board stalled before winning: {}",
                snap.summary
            );
            for &m in &snap.mine_cells {
                board.set_flag(m, true);
            }
            for &s in &snap.safe_cells {
                board.reveal(s);
            }
        }
        assert_eq!(board.status, GameStatus::Won);
    }
}
