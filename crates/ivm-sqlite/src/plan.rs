//! Program SQL: a small SELECT grammar compiled to a supported plan.
//!
//! Supported shapes, everything else an explicit [`ErrorKind::Unsupported`]:
//!
//! - `SELECT keys, count(*) AS n, sum(col) AS total FROM t GROUP BY keys`
//!   (the aggregate root),
//! - `SELECT ... FROM t [JOIN u ON a=b [AND ...]]` (inner equi-join over
//!   source tables),
//! - any number of such branches under set `UNION`.
//!
//! No rule names, no term arenas: the plan speaks in tables, columns and
//! operators only.

use crate::error::{EngineError, ErrorKind, Stage};
use crate::OutputColumn;
use ivm_ir::{Agg, ColId, Expr, Func, NodeId, Op, Program as IrProgram, RelId, RelKind, Relation, Stratum, Ty};

/// The compiled plan. Field-for-field the storage layout: one staging table
/// per scan, one derivation-delta table per join, one root table.
pub(crate) struct Compiled {
    pub root: Root,
    pub scans: Vec<ScanSpec>,
    pub joins: Vec<JoinSpec>,
    /// Output schema in output order. Value columns carry no declared type:
    /// they store storage classes as-is, exactly like the source cells.
    pub output: Vec<OutputColumn>,
    /// Distinct source tables, in first-use order: what the collector watches.
    pub sources: Vec<String>,
    /// Widest source arity: sizes the batch staging table.
    pub stage_width: usize,
}

pub(crate) enum Root {
    /// Set-union root: weight per output row = total derivation support.
    Union { branches: Vec<BranchRef> },
    /// Aggregate root over one scan.
    Group {
        scan: usize,
        keys: Vec<KeysOf>,
        sums: Vec<SumOf>,
    },
}

pub(crate) enum BranchRef {
    /// A scan branch: output columns taken from the scan's needed columns.
    Scan { scan: usize, takes: Vec<usize> },
    /// A scalar filter/map/projection over one source scan's delta.
    MfpScan { scan: usize, select: Vec<String>, filters: Vec<String> },
    MfpJoin { join: usize, select: Vec<String>, filters: Vec<String> },
    /// A join branch: its output order is the union output order.
    Join(usize),
}

fn render_expr(expr: &Expr, columns: &[String], maps: &[String]) -> Option<String> {
    Some(match expr {
        Expr::Col(col) if (*col as usize) < columns.len() => quote_ident(&columns[*col as usize]),
        Expr::Col(col) => maps.get(*col as usize - columns.len())?.clone(),
        Expr::Lit(v) if *v == i64::MIN => "(-9223372036854775807 - 1)".into(),
        Expr::Lit(v) => format!("({v})"),
        Expr::Call(func, args) => {
            let a = |i: usize| render_expr(args.get(i)?, columns, maps);
            let bin = |op: &str| Some(format!("(({}) {op} ({}))", a(0)?, a(1)?));
            match func {
                Func::Eq => bin("=")?,
                Func::Ne => bin("<>")?,
                Func::Lt => bin("<")?,
                Func::Le => bin("<=")?,
                Func::Gt => bin(">")?,
                Func::Ge => bin(">=")?,
                Func::Add | Func::Sub => {
                    let (x, y) = (a(0)?, a(1)?);
                    let min = "(-9223372036854775807 - 1)";
                    let max = "9223372036854775807";
                    match func {
                        Func::Add => format!(
                            "(CASE WHEN {y} > 0 AND {x} > {max} - {y} THEN ({x} + {min}) + ({y} + {min}) \
                             WHEN {y} < 0 AND {x} < {min} - {y} THEN ({x} - {min}) + ({y} - {min}) \
                             ELSE {x} + {y} END)"
                        ),
                        Func::Sub => format!(
                            "(CASE WHEN {y} < 0 AND {x} > {max} + {y} THEN ({x} + {min}) - ({y} - {min}) \
                             WHEN {y} > 0 AND {x} < {min} + {y} THEN ({x} - {min}) - ({y} + {min}) \
                             ELSE {x} - {y} END)"
                        ),
                        _ => unreachable!(),
                    }
                }
                Func::And => format!("((({}) <> 0) AND (({}) <> 0))", a(0)?, a(1)?),
                Func::Or => format!("((({}) <> 0) OR (({}) <> 0))", a(0)?, a(1)?),
                Func::Not => format!("(({}) = 0)", a(0)?),
            }
        }
    })
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn render_mfp(
    columns: &[String],
    filter: &[Expr],
    map: &[Expr],
    project: &[ColId],
) -> Option<(Vec<String>, Vec<String>)> {
    let mut maps = Vec::new();
    for expr in map {
        maps.push(render_expr(expr, columns, &maps)?);
    }
    let filters = filter.iter().map(|expr| {
        Some(format!("({}) <> 0", render_expr(expr, columns, &[])?))
    }).collect::<Option<Vec<_>>>()?;
    let projected: Vec<ColId> = if project.is_empty() {
        (0..columns.len() + maps.len()).map(|i| i as ColId).collect()
    } else { project.to_vec() };
    let select = projected.iter().map(|&col| render_expr(&Expr::Col(col), columns, &maps))
        .collect::<Option<Vec<_>>>()?;
    Some((select, filters))
}

/// Group keys resolved into the input scan's needed columns.
pub(crate) struct KeysOf {
    pub take: usize,
    pub name: String,
}

pub(crate) struct SumOf {
    pub take: usize,
    pub name: String,
}

pub(crate) struct ScanSpec {
    pub table: String,
    /// All columns of the source table, declared order.
    pub columns: Vec<String>,
    /// The deduped subset the plan reads, arbitrary stable order.
    pub needed: Vec<String>,
    /// Engine staging table holding this scan's netted frontier delta.
    pub stage: String,
}

pub(crate) struct JoinSpec {
    pub left: usize,
    pub right: usize,
    /// Equi-conjunct key columns, table names, aligned pair-for-pair.
    pub left_key: Vec<String>,
    pub right_key: Vec<String>,
    /// Projected output positions into each side's `needed` columns.
    pub left_proj: Vec<usize>,
    pub right_proj: Vec<usize>,
    /// Output column names, left projection first then right.
    pub out_names: Vec<String>,
    /// Engine staging table holding this join's net derivation delta.
    pub delta: String,
}

/// Resolves a table name to its declared column list; `None` when absent.
pub(crate) type Schema<'a> = dyn Fn(&str) -> Option<Vec<String>> + 'a;

/// Compile one program. The shape decides the root: a single SELECT with
/// GROUP BY compiles to the aggregate root; anything else compiles to the
/// union root (a bare SELECT is a one-branch union).
pub(crate) fn compile(
    program: &str,
    sql: &str,
    schema: &Schema<'_>,
) -> Result<Compiled, EngineError> {
    let toks = lex(sql)?;
    let mut parser = Parser { toks, pos: 0 };
    let branches = parser.parse_program()?;
    if branches.len() == 1 && branches[0].group.is_some() {
        compile_group(program, &branches[0], schema)
    } else {
        compile_union(program, &branches, schema)
    }
}

fn compile_union(
    program: &str,
    branches: &[AstSelect],
    schema: &Schema<'_>,
) -> Result<Compiled, EngineError> {
    let mut c = Compiler {
        program,
        schema,
        scans: Vec::new(),
        joins: Vec::new(),
        sources: Vec::new(),
    };
    let output = branches[0].item_names()?;
    let mut refs = Vec::new();
    for branch in branches {
        if branch.group.is_some() {
            return Err(EngineError::unsupported(
                Stage::Plan,
                program,
                "GROUP BY under UNION is not a supported shape",
            ));
        }
        if branch.item_names()? != output {
            return Err(EngineError::unsupported(
                Stage::Plan,
                program,
                "UNION branches must select the same columns in the same order",
            ));
        }
        refs.push(c.branch(branch)?);
    }
    finish(c, Root::Union { branches: refs }, output)
}

fn compile_group(
    program: &str,
    select: &AstSelect,
    schema: &Schema<'_>,
) -> Result<Compiled, EngineError> {
    let mut c = Compiler {
        program,
        schema,
        scans: Vec::new(),
        joins: Vec::new(),
        sources: Vec::new(),
    };
    let group = select.group.as_ref().unwrap();
    let Branch::Table(table) = c.from(&select.from)? else {
        return Err(EngineError::unsupported(
            Stage::Plan,
            program,
            "GROUP BY over a join is not a supported shape",
        ));
    };
    let scan = table.scan;
    let mut keys = Vec::new();
    for r in group {
        let take = c.resolve_into(scan, &table.alias, r)?;
        keys.push(KeysOf {
            take,
            name: c.scans[scan].needed[take].clone(),
        });
    }
    let mut sums = Vec::new();
    let mut output: Vec<OutputColumn> = Vec::new();
    for key in &keys {
        output.push(OutputColumn {
            name: key.name.clone(),
        });
    }
    for item in &select.items {
        match item {
            AstItem::Col { r, .. } => {
                if !keys.iter().any(|k| c.scans[scan].needed[k.take] == r.name) {
                    return Err(EngineError::unsupported(
                        Stage::Plan,
                        program,
                        "a plain select item must be a GROUP BY key",
                    ));
                }
            }
            AstItem::Count { alias } => {
                output.push(OutputColumn {
                    name: alias.clone(),
                });
            }
            AstItem::Sum { r, alias } => {
                let take = c.resolve_into(scan, &table.alias, r)?;
                sums.push(SumOf {
                    take,
                    name: alias.clone(),
                });
                output.push(OutputColumn {
                    name: alias.clone(),
                });
            }
        }
    }
    finish(c, Root::Group { scan, keys, sums }, output)
}
fn finish(c: Compiler<'_>, root: Root, output: Vec<OutputColumn>) -> Result<Compiled, EngineError> {
    let mut c = c;
    for scan in &mut c.scans {
        if scan.needed.is_empty() {
            return Err(EngineError::unsupported(
                Stage::Plan,
                &scan.table,
                "a branch must select at least one column",
            ));
        }
    }
    let stage_width = c
        .sources
        .iter()
        .filter_map(|t| (c.schema)(t).map(|cols| cols.len()))
        .max()
        .unwrap_or(0);
    Ok(Compiled {
        root,
        scans: c.scans,
        joins: c.joins,
        output,
        sources: c.sources,
        stage_width,
    })
}

/// Lower the supported SQL plan into the persisted engine-independent program.
/// Source column names are resolved from the database again when the IR is compiled.
pub(crate) fn lower_ir(plan: &Compiled) -> Result<IrProgram, EngineError> {
    let mut rels = Vec::new();
    for (id, name) in plan.sources.iter().enumerate() {
        let columns = plan.scans.iter().find(|scan| &scan.table == name)
            .expect("every source has a scan");
        rels.push(Relation {
            id: id as RelId,
            name: name.clone(),
            cols: vec![Ty::Id; columns.columns.len()],
            kind: RelKind::Source,
        });
    }
    let output_id = rels.len() as RelId;
    rels.push(Relation {
        id: output_id,
        name: String::new(),
        cols: vec![Ty::Id; plan.output.len()],
        kind: RelKind::Derived,
    });
    let mut nodes = Vec::new();
    let mut scan_nodes = Vec::new();
    for scan in &plan.scans {
        let source = plan.sources.iter().position(|name| name == &scan.table)
            .expect("scan source is registered") as RelId;
        let get = nodes.len() as NodeId;
        nodes.push(Op::Get(source));
        let project = scan.needed.iter().map(|name| {
            scan.columns.iter().position(|column| column == name)
                .expect("needed column is in source") as ColId
        }).collect();
        let scan_node = nodes.len() as NodeId;
        nodes.push(Op::Mfp { input: get, filter: Vec::new(), map: Vec::new(), project });
        scan_nodes.push(scan_node);
    }
    let body = match &plan.root {
        Root::Union { branches } => {
            let mut branch_nodes = Vec::new();
            for branch in branches {
                let (input, project) = match branch {
                    BranchRef::Scan { scan, takes } => {
                        (scan_nodes[*scan], takes.iter().map(|&i| i as ColId).collect())
                    }
                    BranchRef::MfpScan { .. } | BranchRef::MfpJoin { .. } => {
                        return Err(EngineError::unsupported(Stage::Plan, "", "SQL lowering cannot reconstruct an MfpScan"));
                    }
                    BranchRef::Join(join_id) => {
                        let join = &plan.joins[*join_id];
                        let left = &plan.scans[join.left];
                        let right = &plan.scans[join.right];
                        let equivalences = join.left_key.iter().zip(&join.right_key).map(|(l, r)| {
                            vec![
                                (0, left.needed.iter().position(|name| name == l).expect("join key selected") as ColId),
                                (1, right.needed.iter().position(|name| name == r).expect("join key selected") as ColId),
                            ]
                        }).collect();
                        let join_node = nodes.len() as NodeId;
                        nodes.push(Op::Join {
                            inputs: vec![scan_nodes[join.left], scan_nodes[join.right]],
                            equivalences,
                        });
                        let mut project: Vec<ColId> = join.left_proj.iter().map(|&i| i as ColId).collect();
                        project.extend(join.right_proj.iter().map(|&i| (left.needed.len() + i) as ColId));
                        (join_node, project)
                    }
                };
                let branch_node = nodes.len() as NodeId;
                nodes.push(Op::Mfp { input, filter: Vec::new(), map: Vec::new(), project });
                branch_nodes.push(branch_node);
            }
            let root = nodes.len() as NodeId;
            nodes.push(Op::Union(branch_nodes));
            root
        }
        Root::Group { scan, keys, sums } => {
            let root = nodes.len() as NodeId;
            let mut aggs = vec![Agg::Count];
            aggs.extend(sums.iter().map(|sum| Agg::Sum(sum.take as ColId)));
            nodes.push(Op::Reduce {
                input: scan_nodes[*scan],
                key: keys.iter().map(|key| key.take as ColId).collect(),
                aggs,
            });
            root
        }
    };
    Ok(IrProgram {
        rels,
        nodes,
        strata: vec![Stratum::Let { id: output_id, body }],
        outputs: vec![output_id],
    })
}

/// Compile the persisted IR back into the existing scan/join/root layout.
/// Later operator groups extend this match without adding a second catalog format.
pub(crate) fn compile_ir(
    name: &str,
    ir: &IrProgram,
    output: Vec<OutputColumn>,
    schema: &Schema<'_>,
) -> Result<Compiled, EngineError> {
    let unsupported = || EngineError::unsupported(Stage::Plan, name, "IR operator outside the SQL frontier subset");
    let &[output_id] = ir.outputs.as_slice() else { return Err(unsupported()); };
    let [Stratum::Let { id, body }] = ir.strata.as_slice() else { return Err(unsupported()); };
    if *id != output_id { return Err(unsupported()); }
    let mut scans = Vec::new();
    let mut scan_nodes = Vec::<(NodeId, usize)>::new();
    let mut joins = Vec::new();

    fn scan_at(
        id: NodeId,
        ir: &IrProgram,
        schema: &Schema<'_>,
        scans: &mut Vec<ScanSpec>,
        scan_nodes: &mut Vec<(NodeId, usize)>,
    ) -> Option<usize> {
        if let Some((_, index)) = scan_nodes.iter().find(|(node, _)| *node == id) {
            return Some(*index);
        }
        let (rel, projected) = match ir.nodes.get(id as usize)? {
            Op::Get(rel) => (*rel, None),
            Op::Mfp { input, filter, map, project } if filter.is_empty() && map.is_empty() => {
                let Op::Get(rel) = ir.nodes.get(*input as usize)? else { return None; };
                (*rel, Some(project))
            }
            _ => return None,
        };
        let relation = ir.rels.iter().find(|r| r.id == rel && r.kind == RelKind::Source)?;
        let columns = schema(&relation.name)?;
        let project: Vec<usize> = match projected {
            Some(project) if !project.is_empty() => project.iter().map(|&p| p as usize).collect(),
            _ => (0..columns.len()).collect(),
        };
        let needed = project.iter().map(|&p| columns.get(p).cloned()).collect::<Option<Vec<_>>>()?;
        let index = scans.len();
        scans.push(ScanSpec { table: relation.name.clone(), columns, needed, stage: String::new() });
        scan_nodes.push((id, index));
        Some(index)
    }

    let root_node = match ir.nodes.get(*body as usize).ok_or_else(unsupported)? {
        Op::Threshold(input) => *input,
        _ => *body,
    };
    let root_op = ir.nodes.get(root_node as usize).ok_or_else(unsupported)?;
    let wrapped = matches!(root_op, Op::Get(_) | Op::Mfp { .. })
        .then(|| Op::Union(vec![root_node]));
    let mut branches = Vec::new();
    let root = match wrapped.as_ref().unwrap_or(root_op) {
        Op::Union(inputs) => {
            for &branch in inputs {
                if matches!(ir.nodes.get(branch as usize), Some(Op::Get(_))) {
                    let scan = scan_at(branch, ir, schema, &mut scans, &mut scan_nodes).ok_or_else(unsupported)?;
                    branches.push(BranchRef::Scan { scan, takes: (0..scans[scan].needed.len()).collect() });
                    continue;
                }
                let Op::Mfp { input, filter, map, project } = ir.nodes.get(branch as usize).ok_or_else(unsupported)?
                    else { return Err(unsupported()); };
                match ir.nodes.get(*input as usize).ok_or_else(unsupported)? {
                    Op::Get(_) | Op::Mfp { .. } => {
                        let scan = scan_at(*input, ir, schema, &mut scans, &mut scan_nodes).ok_or_else(unsupported)?;
                        let columns = &scans[scan].needed;
                        if filter.is_empty() && map.is_empty() {
                            let takes = if project.is_empty() { (0..columns.len()).collect() }
                                else { project.iter().map(|&p| p as usize).collect() };
                            branches.push(BranchRef::Scan { scan, takes });
                        } else {
                            let (select, filters) = render_mfp(columns, filter, map, project).ok_or_else(unsupported)?;
                            if select.len() != output.len() { return Err(unsupported()); }
                            branches.push(BranchRef::MfpScan { scan, select, filters });
                        }
                    }
                    Op::Join { inputs, equivalences } if inputs.len() == 2 => {
                        let left = scan_at(inputs[0], ir, schema, &mut scans, &mut scan_nodes).ok_or_else(unsupported)?;
                        let right = scan_at(inputs[1], ir, schema, &mut scans, &mut scan_nodes).ok_or_else(unsupported)?;
                        let (l, r) = (&scans[left], &scans[right]);
                        let mut left_key = Vec::new();
                        let mut right_key = Vec::new();
                        for eq in equivalences {
                            let &[(0, lc), (1, rc)] = eq.as_slice() else { return Err(unsupported()); };
                            left_key.push(l.needed.get(lc as usize).ok_or_else(unsupported)?.clone());
                            right_key.push(r.needed.get(rc as usize).ok_or_else(unsupported)?.clone());
                        }
                        let left_width = l.needed.len();
                        let (left_proj, right_proj, out_names) = if filter.is_empty() && map.is_empty() {
                            let mut left_proj = Vec::new();
                            let mut right_proj = Vec::new();
                            for &col in project {
                                let col = col as usize;
                                if col < left_width { left_proj.push(col); }
                                else { right_proj.push(col - left_width); }
                            }
                            (left_proj, right_proj, output.iter().map(|c| c.name.clone()).collect())
                        } else {
                            let width = left_width + r.needed.len();
                            ((0..left_width).collect(), (0..r.needed.len()).collect(),
                             (0..width).map(|i| format!("v{i}")).collect())
                        };
                        let index = joins.len();
                        joins.push(JoinSpec {
                            left, right, left_key, right_key, left_proj, right_proj,
                            out_names,
                            delta: String::new(),
                        });
                        if filter.is_empty() && map.is_empty() {
                            branches.push(BranchRef::Join(index));
                        } else {
                            let (select, filters) = render_mfp(&joins[index].out_names, filter, map, project).ok_or_else(unsupported)?;
                            if select.len() != output.len() { return Err(unsupported()); }
                            branches.push(BranchRef::MfpJoin { join: index, select, filters });
                        }
                    }
                    _ => return Err(unsupported()),
                }
            }
            Root::Union { branches }
        }
        Op::Reduce { input, key, aggs } => {
            let scan = scan_at(*input, ir, schema, &mut scans, &mut scan_nodes).ok_or_else(unsupported)?;
            if !matches!(aggs.first(), Some(Agg::Count)) { return Err(unsupported()); }
            let keys = key.iter().map(|&take| {
                let take = take as usize;
                Ok(KeysOf { take, name: scans[scan].needed.get(take).ok_or_else(unsupported)?.clone() })
            }).collect::<Result<Vec<_>, EngineError>>()?;
            let sums = aggs[1..].iter().enumerate().map(|(i, agg)| {
                let Agg::Sum(take) = agg else { return Err(unsupported()); };
                Ok(SumOf {
                    take: *take as usize,
                    name: output.get(key.len() + i + 1).ok_or_else(unsupported)?.name.clone(),
                })
            }).collect::<Result<Vec<_>, EngineError>>()?;
            Root::Group { scan, keys, sums }
        }
        _ => return Err(unsupported()),
    };
    let sources = ir.rels.iter().filter(|r| r.kind == RelKind::Source).map(|r| r.name.clone()).collect();
    let stage_width = scans.iter().map(|scan| scan.columns.len()).max().unwrap_or(0);
    Ok(Compiled { root, scans, joins, output, sources, stage_width })
}

struct Compiler<'a> {
    program: &'a str,
    schema: &'a Schema<'a>,
    scans: Vec<ScanSpec>,
    joins: Vec<JoinSpec>,
    sources: Vec<String>,
}

enum Branch {
    Table(ScanRef),
    Join(usize),
}

struct ScanRef {
    scan: usize,
    alias: String,
}

impl<'a> Compiler<'a> {
    fn branch(&mut self, select: &AstSelect) -> Result<BranchRef, EngineError> {
        match self.from(&select.from)? {
            Branch::Table(t) => {
                let mut takes = Vec::new();
                for item in &select.items {
                    let AstItem::Col { r, .. } = item else {
                        return Err(EngineError::unsupported(
                            Stage::Plan,
                            self.program,
                            "aggregates are only valid under GROUP BY",
                        ));
                    };
                    takes.push(self.resolve_into(t.scan, &t.alias, r)?);
                }
                Ok(BranchRef::Scan {
                    scan: t.scan,
                    takes,
                })
            }
            Branch::Join(j) => {
                let AstFrom::Join { left, right, .. } = &select.from else {
                    unreachable!("join branch without a join FROM");
                };
                let (l_name, l_alias) = match &**left {
                    AstFrom::Table { name, alias } => {
                        (name, alias.clone().unwrap_or_else(|| name.clone()))
                    }
                    _ => unreachable!(),
                };
                let (_r_name, r_alias) = match &**right {
                    AstFrom::Table { name, alias } => {
                        (name, alias.clone().unwrap_or_else(|| name.clone()))
                    }
                    _ => unreachable!(),
                };
                let _ = l_name;
                for item in &select.items {
                    let AstItem::Col { r, .. } = item else {
                        return Err(EngineError::unsupported(
                            Stage::Plan,
                            self.program,
                            "aggregates are only valid under GROUP BY",
                        ));
                    };
                    self.project_into_join(j, &l_alias, &r_alias, r)?;
                }
                self.joins[j].out_names =
                    select.item_names()?.into_iter().map(|c| c.name).collect();
                Ok(BranchRef::Join(j))
            }
        }
    }

    fn from(&mut self, from: &AstFrom) -> Result<Branch, EngineError> {
        match from {
            AstFrom::Table { name, alias } => {
                if !self.sources.iter().any(|t| t == name) {
                    self.sources.push(name.clone());
                }
                let Some(columns) = (self.schema)(name) else {
                    return Err(EngineError::new(
                        Stage::Plan,
                        name.clone(),
                        ErrorKind::UnknownRelation(name.clone()),
                    ));
                };
                let index = self.scans.len();
                self.scans.push(ScanSpec {
                    table: name.clone(),
                    columns,
                    needed: Vec::new(),
                    stage: String::new(),
                });
                Ok(Branch::Table(ScanRef {
                    scan: index,
                    alias: alias.clone().unwrap_or_else(|| name.clone()),
                }))
            }
            AstFrom::Join { left, right, on } => {
                let Branch::Table(l) = self.from(left)? else {
                    return Err(EngineError::unsupported(
                        Stage::Plan,
                        self.program,
                        "a join input must be a source table",
                    ));
                };
                let Branch::Table(r) = self.from(right)? else {
                    return Err(EngineError::unsupported(
                        Stage::Plan,
                        self.program,
                        "a join input must be a source table",
                    ));
                };
                let mut left_key = Vec::new();
                let mut right_key = Vec::new();
                for (a, b) in on {
                    let a_side = self.locate(&l, &r, a)?;
                    let b_side = self.locate(&l, &r, b)?;
                    let (lk, rk) = match (a_side, b_side) {
                        (Side::Left(x), Side::Right(y)) | (Side::Right(y), Side::Left(x)) => (x, y),
                        _ => {
                            return Err(EngineError::unsupported(
                                Stage::Plan,
                                self.program,
                                "join conditions must pair one column from each side",
                            ));
                        }
                    };
                    // The key columns must exist in each scan's staging table,
                    // or the settle-time join delta has nothing to probe.
                    self.need_column(l.scan, &lk)?;
                    self.need_column(r.scan, &rk)?;
                    left_key.push(lk);
                    right_key.push(rk);
                }
                if left_key.is_empty() {
                    return Err(EngineError::unsupported(
                        Stage::Plan,
                        self.program,
                        "joins need at least one equi-join condition",
                    ));
                }
                let index = self.joins.len();
                self.joins.push(JoinSpec {
                    left: l.scan,
                    right: r.scan,
                    left_key,
                    right_key,
                    left_proj: Vec::new(),
                    right_proj: Vec::new(),
                    out_names: Vec::new(),
                    delta: String::new(),
                });
                Ok(Branch::Join(index))
            }
        }
    }

    /// Adds one already-resolved table column to a scan's staging layout.
    fn need_column(&mut self, scan: usize, column: &str) -> Result<usize, EngineError> {
        let spec = &mut self.scans[scan];
        if let Some(pos) = spec.needed.iter().position(|c| c == column) {
            return Ok(pos);
        }
        if !spec.columns.iter().any(|c| c == column) {
            return Err(EngineError::new(
                Stage::Plan,
                spec.table.clone(),
                ErrorKind::UnknownColumn(column.to_string()),
            ));
        }
        spec.needed.push(column.to_string());
        Ok(spec.needed.len() - 1)
    }

    /// Appends `r`'s column to the scan's needed set, returning its position.
    fn resolve_into(&mut self, scan: usize, alias: &str, r: &AstRef) -> Result<usize, EngineError> {
        let column = self.column_of(scan, alias, r)?;
        let spec = &mut self.scans[scan];
        if let Some(pos) = spec.needed.iter().position(|c| c == &column) {
            return Ok(pos);
        }
        if !spec.columns.contains(&column) {
            let qualified = match &r.qualifier {
                Some(q) => format!("{q}.{}", r.name),
                None => r.name.clone(),
            };
            return Err(EngineError::new(
                Stage::Plan,
                spec.table.clone(),
                ErrorKind::UnknownColumn(qualified),
            ));
        }
        spec.needed.push(column);
        Ok(spec.needed.len() - 1)
    }

    fn column_of(&self, scan: usize, alias: &str, r: &AstRef) -> Result<String, EngineError> {
        if let Some(q) = &r.qualifier {
            // The FROM scope of a supported shape has one table per scan, so a
            // qualifier is accepted when it names that table; case-insensitive
            // like SQLite identifiers.
            let table = &self.scans[scan].table;
            if !q.eq_ignore_ascii_case(table) && !q.eq_ignore_ascii_case(alias) {
                return Err(EngineError::new(
                    Stage::Plan,
                    table.clone(),
                    ErrorKind::UnknownRelation(q.clone()),
                ));
            }
        }
        Ok(r.name.clone())
    }

    fn project_into_join(
        &mut self,
        join: usize,
        l_alias: &str,
        r_alias: &str,
        r: &AstRef,
    ) -> Result<(), EngineError> {
        let (left, right) = (self.joins[join].left, self.joins[join].right);
        let side = match &r.qualifier {
            Some(q)
                if q.eq_ignore_ascii_case(&self.scans[left].table)
                    || q.eq_ignore_ascii_case(l_alias) =>
            {
                0
            }
            Some(q)
                if q.eq_ignore_ascii_case(&self.scans[right].table)
                    || q.eq_ignore_ascii_case(r_alias) =>
            {
                1
            }
            Some(q) => {
                return Err(EngineError::new(
                    Stage::Plan,
                    self.program,
                    ErrorKind::UnknownRelation(q.clone()),
                ));
            }
            None => {
                let in_left = self.scans[left].columns.iter().any(|c| c == &r.name);
                let in_right = self.scans[right].columns.iter().any(|c| c == &r.name);
                match (in_left, in_right) {
                    (true, false) => 0,
                    (false, true) => 1,
                    (true, true) => {
                        return Err(EngineError::unsupported(
                            Stage::Plan,
                            self.program,
                            "an unqualified join select item is ambiguous; qualify it with each side's table or alias",
                        ));
                    }
                    (false, false) => {
                        return Err(EngineError::new(
                            Stage::Plan,
                            self.program,
                            ErrorKind::UnknownColumn(r.name.clone()),
                        ));
                    }
                }
            }
        };
        if side == 0 {
            let take = self.resolve_into(left, l_alias, r)?;
            self.joins[join].left_proj.push(take);
        } else {
            let take = self.resolve_into(right, r_alias, r)?;
            self.joins[join].right_proj.push(take);
        }
        Ok(())
    }

    /// Which side of a join a reference belongs to, resolved to a table column.
    fn locate(&self, l: &ScanRef, r: &ScanRef, c: &AstRef) -> Result<Side, EngineError> {
        if let Some(q) = &c.qualifier {
            if q.eq_ignore_ascii_case(&l.alias) || q.eq_ignore_ascii_case(&self.scans[l.scan].table)
            {
                return Ok(Side::Left(c.name.clone()));
            }
            if q.eq_ignore_ascii_case(&r.alias) || q.eq_ignore_ascii_case(&self.scans[r.scan].table)
            {
                return Ok(Side::Right(c.name.clone()));
            }
            return Err(EngineError::new(
                Stage::Plan,
                self.program,
                ErrorKind::UnknownRelation(q.clone()),
            ));
        }
        let in_left = self.scans[l.scan].columns.iter().any(|x| x == &c.name);
        let in_right = self.scans[r.scan].columns.iter().any(|x| x == &c.name);
        match (in_left, in_right) {
            (true, false) => Ok(Side::Left(c.name.clone())),
            (false, true) => Ok(Side::Right(c.name.clone())),
            _ => Err(EngineError::new(
                Stage::Plan,
                self.program,
                ErrorKind::UnknownColumn(c.name.clone()),
            )),
        }
    }
}

enum Side {
    Left(String),
    Right(String),
}

// ---------------------------------------------------------------------------
// AST

struct AstSelect {
    items: Vec<AstItem>,
    from: AstFrom,
    group: Option<Vec<AstRef>>,
}

enum AstItem {
    Col { r: AstRef, alias: String },
    Count { alias: String },
    Sum { r: AstRef, alias: String },
}

enum AstFrom {
    Table {
        name: String,
        alias: Option<String>,
    },
    Join {
        left: Box<AstFrom>,
        right: Box<AstFrom>,
        on: Vec<(AstRef, AstRef)>,
    },
}

struct AstRef {
    qualifier: Option<String>,
    name: String,
}

impl AstSelect {
    fn item_names(&self) -> Result<Vec<OutputColumn>, EngineError> {
        Ok(self
            .items
            .iter()
            .map(|item| match item {
                AstItem::Col { r, alias } => OutputColumn {
                    name: if alias.is_empty() {
                        r.name.clone()
                    } else {
                        alias.clone()
                    },
                },
                AstItem::Count { alias } => OutputColumn {
                    name: alias.clone(),
                },
                AstItem::Sum { alias, .. } => OutputColumn {
                    name: alias.clone(),
                },
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Tokenizer

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Quoted(String),
    Number(String),
    Str(String),
    Sym(&'static str),
}

fn lex(sql: &str) -> Result<Vec<Tok>, EngineError> {
    let mut out = Vec::new();
    let mut rest = sql;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace());
        if rest.is_empty() {
            break;
        }
        if let Some(stripped) = rest.strip_prefix("--") {
            let end = stripped.find('\n').map(|i| i + 1).unwrap_or(stripped.len());
            rest = &stripped[end..];
            continue;
        }
        if let Some(stripped) = rest.strip_prefix("/*") {
            let Some(end) = stripped.find("*/") else {
                return Err(EngineError::new(
                    Stage::Parse,
                    "",
                    ErrorKind::Unsupported("unterminated comment"),
                ));
            };
            rest = &stripped[end + 2..];
            continue;
        }
        let first = rest.chars().next().unwrap();
        match first {
            '\'' => {
                let (tok, tail) = take_quoted(&rest[1..], '\'')?;
                out.push(Tok::Str(tok));
                rest = tail;
            }
            '"' | '`' => {
                let (tok, tail) = take_quoted(&rest[1..], first)?;
                out.push(Tok::Quoted(tok));
                rest = tail;
            }
            '(' => push_sym(&mut out, &mut rest, "("),
            ')' => push_sym(&mut out, &mut rest, ")"),
            ',' => push_sym(&mut out, &mut rest, ","),
            '.' => push_sym(&mut out, &mut rest, "."),
            '*' => push_sym(&mut out, &mut rest, "*"),
            '=' => push_sym(&mut out, &mut rest, "="),
            ';' => push_sym(&mut out, &mut rest, ";"),
            c if c.is_ascii_digit() => {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
                    .unwrap_or(rest.len());
                out.push(Tok::Number(rest[..end].to_string()));
                rest = &rest[end..];
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                out.push(Tok::Ident(rest[..end].to_string()));
                rest = &rest[end..];
            }
            other => {
                return Err(EngineError::new(
                    Stage::Parse,
                    other.to_string(),
                    ErrorKind::Unsupported("character in program SQL"),
                ));
            }
        }
    }
    Ok(out)
}

fn push_sym(out: &mut Vec<Tok>, rest: &mut &str, sym: &'static str) {
    out.push(Tok::Sym(sym));
    *rest = &rest[sym.len()..];
}

fn take_quoted(rest: &str, quote: char) -> Result<(String, &str), EngineError> {
    let mut text = String::new();
    let mut tail = rest;
    loop {
        let Some(c) = tail.chars().next() else {
            return Err(EngineError::new(
                Stage::Parse,
                "",
                ErrorKind::Unsupported("unterminated quoted token"),
            ));
        };
        if c == quote {
            if tail[1..].starts_with(quote) {
                text.push(quote);
                tail = &tail[2..];
            } else {
                return Ok((text, &tail[1..]));
            }
        } else {
            text.push(c);
            tail = &tail[c.len_utf8()..];
        }
    }
}

// ---------------------------------------------------------------------------
// Parser

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

fn keyword(tok: &Tok) -> Option<String> {
    match tok {
        Tok::Ident(w) => Some(w.to_ascii_uppercase()),
        _ => None,
    }
}

/// Words that end an implicit table alias.
const ALIAS_STOPPERS: &[&str] = &[
    "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "NATURAL", "ON", "WHERE", "GROUP", "UNION",
    "HAVING", "ORDER", "LIMIT", "OFFSET",
];

impl Parser {
    fn parse_program(&mut self) -> Result<Vec<AstSelect>, EngineError> {
        let mut selects = vec![self.parse_select()?];
        while self.eat_keyword("UNION") {
            if self.eat_keyword("ALL") {
                return Err(EngineError::unsupported(
                    Stage::Plan,
                    "",
                    "UNION ALL needs multiset output deltas; use UNION",
                ));
            }
            selects.push(self.parse_select()?);
        }
        if self.pos != self.toks.len() {
            return Err(EngineError::unsupported(
                Stage::Parse,
                "",
                "trailing tokens after the program's select statements",
            ));
        }
        Ok(selects)
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(t) if keyword(t).as_deref() == Some(word)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_sym(&mut self, s: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Sym(x)) if *x == s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), EngineError> {
        if self.eat_keyword(word) {
            Ok(())
        } else {
            Err(EngineError::new(
                Stage::Parse,
                word.to_string(),
                ErrorKind::Unsupported("expected keyword missing"),
            ))
        }
    }

    fn expect_sym(&mut self, s: &str) -> Result<(), EngineError> {
        if self.eat_sym(s) {
            Ok(())
        } else {
            Err(EngineError::new(
                Stage::Parse,
                s.to_string(),
                ErrorKind::Unsupported("expected punctuation missing"),
            ))
        }
    }

    fn parse_select(&mut self) -> Result<AstSelect, EngineError> {
        if !self.eat_keyword("SELECT") {
            let verb = match self.peek() {
                Some(Tok::Ident(w)) => w.clone(),
                _ => String::new(),
            };
            return Err(EngineError::unsupported(
                Stage::Parse,
                verb,
                "a program must start with SELECT",
            ));
        }
        if self.eat_keyword("DISTINCT") {
            return Err(EngineError::unsupported(
                Stage::Plan,
                "",
                "DISTINCT is set semantics; the union root already dedups",
            ));
        }
        if self.eat_keyword("ALL") {
            return Err(EngineError::unsupported(
                Stage::Plan,
                "",
                "SELECT ALL is not a supported shape",
            ));
        }
        let mut items = vec![self.parse_item()?];
        while self.eat_sym(",") {
            items.push(self.parse_item()?);
        }
        self.expect_keyword("FROM")?;
        let from = self.parse_from()?;
        if self.eat_keyword("WHERE") {
            return Err(EngineError::unsupported(
                Stage::Plan,
                "",
                "filters are not a supported shape",
            ));
        }
        let group = if self.eat_keyword("GROUP") {
            self.expect_keyword("BY")?;
            let mut refs = vec![self.parse_ref()?];
            while self.eat_sym(",") {
                refs.push(self.parse_ref()?);
            }
            Some(refs)
        } else {
            None
        };
        for word in ["HAVING", "ORDER", "LIMIT", "OFFSET"] {
            if self.eat_keyword(word) {
                return Err(EngineError::unsupported(
                    Stage::Plan,
                    "",
                    "having, ordering and limits are not a supported shape",
                ));
            }
        }
        Ok(AstSelect { items, from, group })
    }

    fn parse_item(&mut self) -> Result<AstItem, EngineError> {
        let item = if self.eat_keyword("COUNT") {
            self.expect_sym("(")?;
            if !self.eat_sym("*") {
                return Err(EngineError::unsupported(
                    Stage::Plan,
                    "",
                    "count(col) is not a supported shape; use count(*)",
                ));
            }
            self.expect_sym(")")?;
            AstItem::Count {
                alias: String::new(),
            }
        } else if self.eat_keyword("SUM") {
            self.expect_sym("(")?;
            let r = self.parse_ref()?;
            self.expect_sym(")")?;
            AstItem::Sum {
                r,
                alias: String::new(),
            }
        } else {
            for word in ["AVG", "MIN", "MAX", "TOTAL", "GROUP_CONCAT"] {
                if self.eat_keyword(word) {
                    return Err(EngineError::unsupported(
                        Stage::Plan,
                        word.to_string(),
                        "only count(*) and sum(col) are supported aggregates",
                    ));
                }
            }
            AstItem::Col {
                r: self.parse_ref()?,
                alias: String::new(),
            }
        };
        self.finish_item(item)
    }

    fn finish_item(&mut self, mut item: AstItem) -> Result<AstItem, EngineError> {
        let alias = if self.eat_keyword("AS") {
            match self.peek().cloned() {
                Some(Tok::Ident(w)) | Some(Tok::Quoted(w)) => {
                    self.pos += 1;
                    w
                }
                other => {
                    return Err(EngineError::new(
                        Stage::Parse,
                        format!("{other:?}"),
                        ErrorKind::Unsupported("alias expected"),
                    ));
                }
            }
        } else {
            String::new()
        };
        match &mut item {
            AstItem::Col { alias: a, .. }
            | AstItem::Count { alias: a }
            | AstItem::Sum { alias: a, .. } => *a = alias,
        }
        Ok(item)
    }

    fn parse_from(&mut self) -> Result<AstFrom, EngineError> {
        let mut from = self.parse_table()?;
        loop {
            if self.eat_keyword("INNER") {
                self.expect_keyword("JOIN")?;
                let right = self.parse_table()?;
                let on = self.parse_on()?;
                from = AstFrom::Join {
                    left: Box::new(from),
                    right: Box::new(right),
                    on,
                };
            } else if self.eat_keyword("JOIN") {
                let right = self.parse_table()?;
                let on = self.parse_on()?;
                from = AstFrom::Join {
                    left: Box::new(from),
                    right: Box::new(right),
                    on,
                };
            } else if self.eat_keyword("LEFT")
                || self.eat_keyword("RIGHT")
                || self.eat_keyword("FULL")
                || self.eat_keyword("CROSS")
                || self.eat_keyword("NATURAL")
            {
                return Err(EngineError::unsupported(
                    Stage::Plan,
                    "",
                    "only inner equi-joins are a supported shape",
                ));
            } else if self.eat_sym(",") {
                return Err(EngineError::unsupported(
                    Stage::Plan,
                    "",
                    "comma joins are not a supported shape; use JOIN .. ON",
                ));
            } else {
                break;
            }
        }
        Ok(from)
    }

    fn parse_table(&mut self) -> Result<AstFrom, EngineError> {
        let name = self.parse_name()?;
        let alias = if self.eat_keyword("AS") {
            Some(self.parse_name()?)
        } else {
            match self.peek() {
                Some(Tok::Ident(w)) if !is_alias_stopper(w) => Some(self.parse_name()?),
                _ => None,
            }
        };
        Ok(AstFrom::Table { name, alias })
    }

    fn parse_on(&mut self) -> Result<Vec<(AstRef, AstRef)>, EngineError> {
        self.expect_keyword("ON")?;
        let mut conds = vec![self.parse_equality()?];
        while self.eat_keyword("AND") {
            conds.push(self.parse_equality()?);
        }
        Ok(conds)
    }

    fn parse_equality(&mut self) -> Result<(AstRef, AstRef), EngineError> {
        let a = self.parse_ref()?;
        self.expect_sym("=")?;
        let b = self.parse_ref()?;
        Ok((a, b))
    }

    fn parse_ref(&mut self) -> Result<AstRef, EngineError> {
        let first = self.parse_name()?;
        if self.eat_sym(".") {
            let second = self.parse_name()?;
            Ok(AstRef {
                qualifier: Some(first),
                name: second,
            })
        } else {
            Ok(AstRef {
                qualifier: None,
                name: first,
            })
        }
    }

    fn parse_name(&mut self) -> Result<String, EngineError> {
        match self.peek().cloned() {
            Some(Tok::Ident(w)) | Some(Tok::Quoted(w)) => {
                self.pos += 1;
                Ok(w)
            }
            other => Err(EngineError::new(
                Stage::Parse,
                format!("{other:?}"),
                ErrorKind::Unsupported("identifier expected"),
            )),
        }
    }
}

/// A table alias is implicit until a word that cannot be one.
fn is_alias_stopper(w: &str) -> bool {
    ALIAS_STOPPERS.contains(&w.to_ascii_uppercase().as_str())
}
