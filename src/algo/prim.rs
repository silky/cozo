/*
 * Copyright 2022, The Cozo Project Authors. Licensed under MPL-2.0.
 */

use std::cmp::Reverse;
use std::collections::BTreeMap;

use miette::Diagnostic;
use miette::Result;
use ordered_float::OrderedFloat;
use priority_queue::PriorityQueue;
use smartstring::{LazyCompact, SmartString};
use thiserror::Error;

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

pub(crate) struct MinimumSpanningTreePrim;

impl AlgoImpl for MinimumSpanningTreePrim {
    fn run(
        &mut self,
        tx: &SessionTx,
        algo: &MagicAlgoApply,
        stores: &BTreeMap<MagicSymbol, InMemRelation>,
        out: &InMemRelation,
        poison: Poison,
    ) -> Result<()> {
        let edges = algo.relation(0)?;
        let (graph, indices, inv_indices, _) =
            edges.convert_edge_to_weighted_graph(true, true, tx, stores)?;
        if graph.is_empty() {
            return Ok(());
        }
        let starting = match algo.relation(1) {
            Err(_) => 0,
            Ok(rel) => {
                let tuple = rel.iter(tx, stores)?.next().ok_or_else(|| {
                    #[derive(Debug, Error, Diagnostic)]
                    #[error("The provided starting nodes relation is empty")]
                    #[diagnostic(code(algo::empty_starting))]
                    struct EmptyStarting(#[label] SourceSpan);

                    EmptyStarting(rel.span())
                })??;
                let dv = &tuple.0[0];
                *inv_indices.get(dv).ok_or_else(|| {
                    #[derive(Debug, Error, Diagnostic)]
                    #[error("The requested starting node {0:?} is not found")]
                    #[diagnostic(code(algo::starting_node_not_found))]
                    struct StartingNodeNotFound(DataValue, #[label] SourceSpan);

                    StartingNodeNotFound(dv.clone(), rel.span())
                })?
            }
        };
        let msp = prim(&graph, starting, poison)?;
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

fn prim(
    graph: &[Vec<(usize, f64)>],
    starting: usize,
    poison: Poison,
) -> Result<Vec<(usize, usize, f64)>> {
    let mut visited = vec![false; graph.len()];
    let mut mst_edges = Vec::with_capacity(graph.len() - 1);
    let mut pq = PriorityQueue::new();

    let mut relax_edges_at_node = |node: usize, pq: &mut PriorityQueue<_, _>| {
        visited[node] = true;
        let edges = &graph[node];
        for (to_node, cost) in edges {
            if visited[*to_node] {
                continue;
            }
            pq.push_increase(*to_node, (Reverse(OrderedFloat(*cost)), node));
        }
    };

    relax_edges_at_node(starting, &mut pq);

    while let Some((to_node, (Reverse(OrderedFloat(cost)), from_node))) = pq.pop() {
        if mst_edges.len() == graph.len() - 1 {
            break;
        }
        mst_edges.push((from_node, to_node, cost));
        relax_edges_at_node(to_node, &mut pq);
        poison.check()?;
    }

    Ok(mst_edges)
}
