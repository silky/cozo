/*
 * Copyright 2022, The Cozo Project Authors. Licensed under MPL-2.0.
 */

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use itertools::Itertools;
use miette::{ensure, Diagnostic, Result};
use thiserror::Error;

use crate::data::program::{
    AlgoRuleArg, NormalFormAlgoOrRules, NormalFormAtom, NormalFormProgram,
    StratifiedNormalFormProgram,
};
use crate::data::symb::{Symbol, PROG_ENTRY};
use crate::parse::SourceSpan;
use crate::query::graph::{
    generalized_kahn, reachable_components, strongly_connected_components, Graph, StratifiedGraph,
};

impl NormalFormAtom {
    fn contained_rules(&self) -> BTreeMap<&Symbol, bool> {
        match self {
            NormalFormAtom::Relation(_)
            | NormalFormAtom::NegatedRelation(_)
            | NormalFormAtom::Predicate(_)
            | NormalFormAtom::Unification(_) => Default::default(),
            NormalFormAtom::Rule(r) => BTreeMap::from([(&r.name, false)]),
            NormalFormAtom::NegatedRule(r) => BTreeMap::from([(&r.name, true)]),
        }
    }
}

fn convert_normal_form_program_to_graph(
    nf_prog: &NormalFormProgram,
) -> StratifiedGraph<&'_ Symbol> {
    let meet_rules: BTreeSet<_> = nf_prog
        .prog
        .iter()
        .filter_map(|(k, ruleset)| match ruleset {
            NormalFormAlgoOrRules::Rules { rules: ruleset } => {
                let has_aggr = ruleset
                    .iter()
                    .any(|rule| rule.aggr.iter().any(|a| a.is_some()));
                let is_meet = has_aggr
                    && ruleset.iter().all(|rule| {
                        rule.aggr.iter().all(|v| match v {
                            None => true,
                            Some((v, _)) => v.is_meet,
                        })
                    });
                if is_meet {
                    Some(k)
                } else {
                    None
                }
            }
            NormalFormAlgoOrRules::Algo { algo: _ } => None,
        })
        .collect();
    let algo_rules: BTreeSet<_> = nf_prog
        .prog
        .iter()
        .filter_map(|(k, ruleset)| match ruleset {
            NormalFormAlgoOrRules::Rules { rules: _ } => None,
            NormalFormAlgoOrRules::Algo { algo: _ } => Some(k),
        })
        .collect();
    nf_prog
        .prog
        .iter()
        .map(|(k, ruleset)| match ruleset {
            NormalFormAlgoOrRules::Rules { rules: ruleset } => {
                let mut ret: BTreeMap<&Symbol, bool> = BTreeMap::default();
                let has_aggr = ruleset
                    .iter()
                    .any(|rule| rule.aggr.iter().any(|a| a.is_some()));
                let is_meet = has_aggr
                    && ruleset.iter().all(|rule| {
                        rule.aggr.iter().all(|v| match v {
                            None => true,
                            Some((v, _)) => v.is_meet,
                        })
                    });
                for rule in ruleset {
                    for atom in &rule.body {
                        let contained = atom.contained_rules();
                        for (found_key, is_negated) in contained {
                            let found_key_is_meet =
                                meet_rules.contains(found_key) && found_key != k;
                            let found_key_is_algo = algo_rules.contains(found_key);
                            match ret.entry(found_key) {
                                Entry::Vacant(e) => {
                                    if has_aggr {
                                        if is_meet && k == found_key {
                                            e.insert(found_key_is_algo || is_negated);
                                        } else {
                                            e.insert(true);
                                        }
                                    } else {
                                        e.insert(
                                            found_key_is_algo || found_key_is_meet || is_negated,
                                        );
                                    }
                                }
                                Entry::Occupied(mut e) => {
                                    let old = *e.get();
                                    let new_val = if has_aggr {
                                        if is_meet && k == found_key {
                                            found_key_is_algo || found_key_is_meet || is_negated
                                        } else {
                                            true
                                        }
                                    } else {
                                        found_key_is_algo || found_key_is_meet || is_negated
                                    };
                                    e.insert(old || new_val);
                                }
                            }
                        }
                    }
                }
                (k, ret)
            }
            NormalFormAlgoOrRules::Algo { algo } => {
                let mut ret: BTreeMap<&Symbol, bool> = BTreeMap::default();
                for rel in &algo.rule_args {
                    match rel {
                        AlgoRuleArg::InMem { name, .. } => {
                            ret.insert(name, true);
                        }
                        AlgoRuleArg::Stored { .. } | AlgoRuleArg::NamedStored { .. } => {}
                    }
                }
                (k, ret)
            }
        })
        .collect()
}

fn reduce_to_graph<'a>(g: &StratifiedGraph<&'a Symbol>) -> Graph<&'a Symbol> {
    g.iter()
        .map(|(k, s)| (*k, s.iter().map(|(sk, _)| *sk).collect_vec()))
        .collect()
}

fn verify_no_cycle(g: &StratifiedGraph<&'_ Symbol>, sccs: &[BTreeSet<&Symbol>]) -> Result<()> {
    for (k, vs) in g {
        for scc in sccs {
            if scc.contains(k) {
                for (v, negated) in vs {
                    #[derive(Debug, Error, Diagnostic)]
                    #[error("Query is unstratifiable")]
                    #[diagnostic(code(eval::unstratifiable))]
                    #[diagnostic(help(
                        "The rule '{0}' is in the strongly connected component {1:?},\n\
                    and is involved in at least one forbidden dependency \n\
                    (negation, non-meet aggregation, or algorithm-application)."
                    ))]
                    struct UnStratifiableProgram(String, Vec<String>);

                    ensure!(
                        !negated || !scc.contains(v),
                        UnStratifiableProgram(
                            v.to_string(),
                            scc.iter().map(|v| v.to_string()).collect_vec()
                        )
                    );
                }
            }
        }
    }
    Ok(())
}

fn make_scc_reduced_graph<'a>(
    sccs: &[BTreeSet<&'a Symbol>],
    graph: &StratifiedGraph<&Symbol>,
) -> (BTreeMap<Symbol, usize>, StratifiedGraph<usize>) {
    let indices = sccs
        .iter()
        .enumerate()
        .flat_map(|(idx, scc)| scc.iter().map(move |k| ((*k).clone(), idx)))
        .collect::<BTreeMap<_, _>>();
    let mut ret: BTreeMap<usize, BTreeMap<usize, bool>> = Default::default();
    for (from, tos) in graph {
        let from_idx = *indices.get(from).unwrap();
        let cur_entry = ret.entry(from_idx).or_default();
        for (to, poisoned) in tos {
            let to_idx = match indices.get(to) {
                Some(i) => *i,
                None => continue,
            };
            if from_idx == to_idx {
                continue;
            }
            match cur_entry.entry(to_idx) {
                Entry::Vacant(e) => {
                    e.insert(*poisoned);
                }
                Entry::Occupied(mut e) => {
                    let old_p = *e.get();
                    e.insert(old_p || *poisoned);
                }
            }
        }
    }
    (indices, ret)
}

impl NormalFormProgram {
    pub(crate) fn stratify(self) -> Result<StratifiedNormalFormProgram> {
        // prerequisite: the program is already in disjunctive normal form
        // 0. build a graph of the program
        let prog_entry: &Symbol = &Symbol::new(PROG_ENTRY, SourceSpan(0, 0));
        let stratified_graph = convert_normal_form_program_to_graph(&self);
        let graph = reduce_to_graph(&stratified_graph);

        // 1. find reachable clauses starting from the query
        let reachable: BTreeSet<_> = reachable_components(&graph, &prog_entry)
            .into_iter()
            .map(|k| (*k).clone())
            .collect();
        // 2. prune the graph of unreachable clauses
        let stratified_graph: StratifiedGraph<_> = stratified_graph
            .into_iter()
            .filter(|(k, _)| reachable.contains(k))
            .collect();
        let graph: Graph<_> = graph
            .into_iter()
            .filter(|(k, _)| reachable.contains(k))
            .collect();
        // 3. find SCC of the clauses
        let sccs: Vec<BTreeSet<&Symbol>> = strongly_connected_components(&graph)?
            .into_iter()
            .map(|scc| scc.into_iter().cloned().collect())
            .collect_vec();
        // 4. for each SCC, verify that no neg/agg edges are present so that it is really stratifiable
        verify_no_cycle(&stratified_graph, &sccs)?;
        // 5. build a reduced graph for the SCC's
        let (invert_indices, reduced_graph) = make_scc_reduced_graph(&sccs, &stratified_graph);
        // 6. topological sort the reduced graph to get a stratification
        let sort_result = generalized_kahn(&reduced_graph, stratified_graph.len());
        let n_strata = sort_result.len();
        let invert_sort_result = sort_result
            .into_iter()
            .enumerate()
            .flat_map(|(stratum, indices)| indices.into_iter().map(move |idx| (idx, stratum)))
            .collect::<BTreeMap<_, _>>();
        // 7. translate the stratification into datalog program
        let mut ret: Vec<NormalFormProgram> = vec![Default::default(); n_strata];
        for (name, ruleset) in self.prog {
            if let Some(scc_idx) = invert_indices.get(&name) {
                if let Some(stratum_idx) = invert_sort_result.get(scc_idx) {
                    let target = ret.get_mut(*stratum_idx).unwrap();
                    target.prog.insert(name, ruleset);
                }
            }
        }

        Ok(StratifiedNormalFormProgram(ret))
    }
}
