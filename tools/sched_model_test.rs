/*
 *
 *       tools/sched_model_test.rs
 *       Host harness for the same scheduler algorithms tested inside the kernel
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */
#![allow(dead_code)]
extern crate alloc;
#[path = "../src/sched/pelt.rs"]
mod pelt;
#[path = "../src/sched/rbtree.rs"]
mod rbtree;
#[path = "../src/sched/eevdf.rs"]
mod eevdf;
#[path = "../src/sched/lb_model.rs"]
mod lb_model;
#[path = "../src/sched/model_tests.rs"]
mod model_tests;
