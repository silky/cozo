/*
 * Copyright 2022, The Cozo Project Authors. Licensed under MPL-2.0.
 */

use std::cmp::Reverse;
use std::collections::BTreeMap;

use itertools::Itertools;
use miette::Result;
use ordered_float::OrderedFloat;
use priority_queue::PriorityQueue;
use smartstring::{LazyCompact, SmartString};

use crate::algo::AlgoImpl;
use crate::data::expr::Expr;
use crate::data::program::{MagicAlgoApply, MagicSymbol};
use crate::data::symb::Symbol;
use crate::data::tuple::Tuple;
use crate::data::value::DataValue;
use crate::parse::SourceSpan;
use crate::runtime::db::Poison;
use crate::runtime::in_mem::InMemRelation;
use crate::runtime::transact::SessionTx;

pub(crate) struct MinimumSpanningForestKruskal;

impl AlgoImpl for MinimumSpanningForestKruskal {
    fn run(
        &mut self,
        tx: &SessionTx,
        algo: &MagicAlgoApply,
        stores: &BTreeMap<MagicSymbol, InMemRelation>,
        out: &InMemRelation,
        poison: Poison,
    ) -> Result<()> {
        let edges = algo.relation(0)?;
        let (graph, indices, _, _) =
            edges.convert_edge_to_weighted_graph(true, true, tx, stores)?;
        if graph.is_empty() {
            return Ok(());
        }
        let msp = kruskal(&graph, poison)?;
        for (src, dst, cost) in msp {
            out.put(
                Tuple(vec![
                    indices[src].clone(),
                    indices[dst].clone(),
                    DataValue::from(cost),
                ]),
                0,
            );
        }

        Ok(())
    }

    fn arity(
        &self,
        _options: &BTreeMap<SmartString<LazyCompact>, Expr>,
        _rule_head: &[Symbol],
        _span: SourceSpan,
    ) -> Result<usize> {
        Ok(3)
    }
}

fn kruskal(edges: &[Vec<(usize, f64)>], poison: Poison) -> Result<Vec<(usize, usize, f64)>> {
    let mut pq = PriorityQueue::new();
    let mut uf = UnionFind::new(edges.len());
    let mut mst = Vec::with_capacity(edges.len() - 1);
    for (from, tos) in edges.iter().enumerate() {
        for (to, cost) in tos {
            pq.push((from, *to), Reverse(OrderedFloat(*cost)));
        }
        poison.check()?;
    }
    while let Some(((from, to), Reverse(OrderedFloat(cost)))) = pq.pop() {
        if uf.connected(from, to) {
            continue;
        }
        uf.union(from, to);

        mst.push((from, to, cost));
        if uf.szs[0] == edges.len() {
            break;
        }
        poison.check()?;
    }
    Ok(mst)
}

struct UnionFind {
    ids: Vec<usize>,
    szs: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            ids: (0..n).collect_vec(),
            szs: vec![1; n],
        }
    }
    fn union(&mut self, p: usize, q: usize) {
        let root1 = self.find(p);
        let root2 = self.find(q);
        if root1 != root2 {
            if self.szs[root1] < self.szs[root2] {
                self.szs[root2] += self.szs[root1];
                self.ids[root1] = root2;
            } else {
                self.szs[root1] += self.szs[root2];
                self.ids[root2] = root1;
            }
        }
    }
    fn find(&mut self, mut p: usize) -> usize {
        let mut root = p;
        while root != self.ids[root] {
            root = self.ids[root];
        }
        while p != root {
            let next = self.ids[p];
            self.ids[p] = root;
            p = next;
        }
        root
    }
    fn connected(&mut self, p: usize, q: usize) -> bool {
        self.find(p) == self.find(q)
    }
}
