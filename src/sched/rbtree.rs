/*
 *
 *       src/sched/rbtree.rs
 *       Allocation-free augmented left-leaning red-black queue operations
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! 2-3 LLRB insertion and top-down deletion, with subtree service aggregates.
//! Algorithm reference: https://algs4.cs.princeton.edu/33balanced/RedBlackBST.java.html
//! Nodes are allocated before admission and returned intact on removal. Rotations,
//! selection and deletion neither allocate nor destroy a task on an IRQ stack.

use super::pelt::LoadAvg;
use alloc::boxed::Box;

pub type Key = (i128, u64);
type Link<V> = Option<Box<Node<V>>>;

pub struct Node<V> {
    pub key: Key,
    pub value: V,
    pub vruntime: i128,
    pub deadline: i128,
    pub weight: u32,
    pub remaining_ns: u64,
    pub remainder: u64,
    pub lag: i128,
    pub last_run_us: u64,
    pub pelt: LoadAvg,
    red: bool,
    left: Link<V>,
    right: Link<V>,
    minimum: i128,
    weighted_sum: i128,
    total_weight: u64,
    count: usize,
}

impl<V> Node<V> {
    pub fn new(tid: u64, value: V, weight: u32) -> Box<Self> {
        let mut node = Box::new(Self {
            key: (0, tid),
            value,
            vruntime: 0,
            deadline: 0,
            weight,
            remaining_ns: 0,
            remainder: 0,
            lag: 0,
            last_run_us: 0,
            pelt: LoadAvg::new(),
            red: true,
            left: None,
            right: None,
            minimum: 0,
            weighted_sum: 0,
            total_weight: 0,
            count: 1,
        });
        node.refresh();
        node
    }

    fn refresh(&mut self) {
        self.minimum = self.vruntime;
        self.weighted_sum = self.vruntime * i128::from(self.weight);
        self.total_weight = u64::from(self.weight);
        self.count = 1;
        for child in [&self.left, &self.right].into_iter().flatten() {
            self.minimum = self.minimum.min(child.minimum);
            self.weighted_sum += child.weighted_sum;
            self.total_weight += child.total_weight;
            self.count += child.count;
        }
    }
}

pub struct RBTree<V> {
    root: Link<V>,
}

impl<V> RBTree<V> {
    pub const fn new() -> Self {
        Self { root: None }
    }
    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.count)
    }
    pub fn weight(&self) -> u64 {
        self.root.as_ref().map_or(0, |n| n.total_weight)
    }
    pub fn weighted_sum(&self) -> i128 {
        self.root.as_ref().map_or(0, |n| n.weighted_sum)
    }
    pub fn minimum_vruntime(&self) -> Option<i128> {
        self.root.as_ref().map(|n| n.minimum)
    }

    pub fn insert(&mut self, mut node: Box<Node<V>>) {
        assert!(node.left.is_none() && node.right.is_none());
        node.red = true;
        node.refresh();
        self.root = Some(Self::insert_at(self.root.take(), node));
        self.root.as_mut().unwrap().red = false;
    }

    fn insert_at(root: Link<V>, node: Box<Node<V>>) -> Box<Node<V>> {
        let Some(mut h) = root else {
            return node;
        };
        match node.key.cmp(&h.key) {
            core::cmp::Ordering::Less => h.left = Some(Self::insert_at(h.left.take(), node)),
            core::cmp::Ordering::Greater => h.right = Some(Self::insert_at(h.right.take(), node)),
            core::cmp::Ordering::Equal => panic!("duplicate scheduler node"),
        }
        Self::balance(h)
    }

    pub fn get(&self, key: Key) -> Option<&Node<V>> {
        let mut link = self.root.as_deref();
        while let Some(node) = link {
            match key.cmp(&node.key) {
                core::cmp::Ordering::Less => link = node.left.as_deref(),
                core::cmp::Ordering::Greater => link = node.right.as_deref(),
                core::cmp::Ordering::Equal => return Some(node),
            }
        }
        None
    }

    pub fn first(&self) -> Option<&Node<V>> {
        let mut node = self.root.as_deref()?;
        while let Some(left) = node.left.as_deref() {
            node = left;
        }
        Some(node)
    }

    /// Earliest deadline whose vruntime <= floor(weighted virtual time).
    /// Subtree minima prune ineligible branches, keeping selection O(log n).
    pub fn eligible(&self, virtual_time: i128) -> Option<&Node<V>> {
        let mut node = self.root.as_deref()?;
        if node.minimum > virtual_time {
            return None;
        }
        loop {
            if node
                .left
                .as_ref()
                .is_some_and(|left| left.minimum <= virtual_time)
            {
                node = node.left.as_deref().unwrap();
            } else if node.vruntime <= virtual_time {
                return Some(node);
            } else {
                node = node.right.as_deref()?;
                if node.minimum > virtual_time {
                    return None;
                }
            }
        }
    }

    /// Bounded reverse-deadline scan for migration candidates.
    pub fn find(
        &self,
        budget: &mut usize,
        predicate: &mut impl FnMut(&Node<V>) -> bool,
    ) -> Option<Key> {
        fn visit<V>(
            link: &Link<V>,
            budget: &mut usize,
            predicate: &mut impl FnMut(&Node<V>) -> bool,
        ) -> Option<Key> {
            let node = link.as_ref()?;
            if *budget == 0 {
                return None;
            }
            if let Some(key) = visit(&node.right, budget, predicate) {
                return Some(key);
            }
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            if predicate(node) {
                return Some(node.key);
            }
            visit(&node.left, budget, predicate)
        }
        visit(&self.root, budget, predicate)
    }

    pub fn for_each(&self, mut f: impl FnMut(&Node<V>)) {
        fn visit_all<V>(node: Option<&Node<V>>, f: &mut impl FnMut(&Node<V>)) {
            if let Some(n) = node {
                visit_all(n.left.as_deref(), f);
                f(n);
                visit_all(n.right.as_deref(), f);
            }
        }
        visit_all(self.root.as_deref(), &mut f);
    }

    pub fn pop_first(&mut self) -> Option<Box<Node<V>>> {
        self.remove(self.first()?.key)
    }

    pub fn remove(&mut self, key: Key) -> Option<Box<Node<V>>> {
        self.get(key)?;
        let mut root = self.root.take().unwrap();
        if !is_red(&root.left) && !is_red(&root.right) {
            root.red = true;
        }
        let (new_root, mut removed) = Self::delete(root, key);
        self.root = new_root;
        if let Some(root) = self.root.as_mut() {
            root.red = false;
        }
        removed.left = None;
        removed.right = None;
        removed.refresh();
        Some(removed)
    }

    fn delete(mut h: Box<Node<V>>, key: Key) -> (Link<V>, Box<Node<V>>) {
        let removed;
        if key < h.key {
            if !is_red(&h.left) && !left_red(&h.left) {
                h = Self::move_red_left(h);
            }
            let (left, found) = Self::delete(h.left.take().unwrap(), key);
            h.left = left;
            removed = found;
        } else {
            if is_red(&h.left) {
                h = Self::rotate_right(h);
            }
            if key == h.key && h.right.is_none() {
                return (None, h);
            }
            if !is_red(&h.right) && !left_red(&h.right) {
                h = Self::move_red_right(h);
            }
            if key == h.key {
                let (right, mut successor) = Self::delete_min(h.right.take().unwrap());
                successor.left = h.left.take();
                successor.right = right;
                successor.red = h.red;
                removed = h;
                h = successor;
            } else {
                let (right, found) = Self::delete(h.right.take().unwrap(), key);
                h.right = right;
                removed = found;
            }
        }
        (Some(Self::balance(h)), removed)
    }

    fn delete_min(mut h: Box<Node<V>>) -> (Link<V>, Box<Node<V>>) {
        if h.left.is_none() {
            return (None, h);
        }
        if !is_red(&h.left) && !left_red(&h.left) {
            h = Self::move_red_left(h);
        }
        let (left, removed) = Self::delete_min(h.left.take().unwrap());
        h.left = left;
        (Some(Self::balance(h)), removed)
    }

    fn rotate_left(mut h: Box<Node<V>>) -> Box<Node<V>> {
        let mut x = h.right.take().unwrap();
        h.right = x.left.take();
        x.red = h.red;
        h.red = true;
        h.refresh();
        x.left = Some(h);
        x.refresh();
        x
    }
    fn rotate_right(mut h: Box<Node<V>>) -> Box<Node<V>> {
        let mut x = h.left.take().unwrap();
        h.left = x.right.take();
        x.red = h.red;
        h.red = true;
        h.refresh();
        x.right = Some(h);
        x.refresh();
        x
    }
    fn flip(h: &mut Node<V>) {
        h.red = !h.red;
        let left = h.left.as_mut().expect("missing left color sibling");
        left.red = !left.red;
        let right = h.right.as_mut().expect("missing right color sibling");
        right.red = !right.red;
    }
    fn move_red_left(mut h: Box<Node<V>>) -> Box<Node<V>> {
        Self::flip(&mut h);
        if left_red(&h.right) {
            h.right = Some(Self::rotate_right(h.right.take().unwrap()));
            h = Self::rotate_left(h);
            Self::flip(&mut h);
        }
        h.refresh();
        h
    }
    fn move_red_right(mut h: Box<Node<V>>) -> Box<Node<V>> {
        Self::flip(&mut h);
        if left_red(&h.left) {
            h = Self::rotate_right(h);
            Self::flip(&mut h);
        }
        h.refresh();
        h
    }
    fn balance(mut h: Box<Node<V>>) -> Box<Node<V>> {
        if is_red(&h.right) && !is_red(&h.left) {
            h = Self::rotate_left(h);
        }
        if is_red(&h.left) && left_red(&h.left) {
            h = Self::rotate_right(h);
        }
        if is_red(&h.left) && is_red(&h.right) {
            Self::flip(&mut h);
        }
        h.refresh();
        h
    }

    #[cfg(any(test, feature = "boot-self-test"))]
    pub fn validate(&self) {
        fn check<V>(
            link: &Link<V>,
            lower: Option<Key>,
            upper: Option<Key>,
        ) -> (usize, usize, i128, u64, i128) {
            let Some(n) = link else {
                return (1, 0, 0, 0, i128::MAX);
            };
            assert!(lower.is_none_or(|key| key < n.key));
            assert!(upper.is_none_or(|key| n.key < key));
            assert!(!is_red(&n.right));
            assert!(!n.red || (!is_red(&n.left) && !is_red(&n.right)));
            let l = check(&n.left, lower, Some(n.key));
            let r = check(&n.right, Some(n.key), upper);
            assert_eq!(l.0, r.0, "red-black black height");
            let count = 1 + l.1 + r.1;
            let sum = n.vruntime * i128::from(n.weight) + l.2 + r.2;
            let weight = u64::from(n.weight) + l.3 + r.3;
            let minimum = n.vruntime.min(l.4).min(r.4);
            assert_eq!(
                (n.count, n.weighted_sum, n.total_weight, n.minimum),
                (count, sum, weight, minimum)
            );
            (l.0 + usize::from(!n.red), count, sum, weight, minimum)
        }
        assert!(self.root.as_ref().is_none_or(|n| !n.red));
        check(&self.root, None, None);
    }
}
fn is_red<V>(link: &Link<V>) -> bool {
    link.as_ref().is_some_and(|n| n.red)
}
fn left_red<V>(link: &Link<V>) -> bool {
    link.as_ref().is_some_and(|n| is_red(&n.left))
}
