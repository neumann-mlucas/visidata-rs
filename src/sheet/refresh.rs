//! `:refresh` / `:link` (S-30 A): rebuild a derived sheet from its parent's
//! current view by replaying its [`Derivation`].

use super::*;

/// A refreshed sheet; `snapshot`: its parent was closed, so it was rebuilt
/// from the copy taken when it was made.
pub struct Refreshed {
    pub sheets: Vec<Sheet>,
    pub snapshot: bool,
}

impl Sheet {
    /// The parent's view as text: what `:link` compares to see a change.
    pub fn plan_key(&self) -> String {
        self.view_plan().describe_plan().unwrap_or_default()
    }

    /// The live parent's view differs from the one this was made from.
    pub fn parent_changed(&self, stack: &[Sheet]) -> bool {
        self.state.lineage.as_ref().is_some_and(|l| {
            stack
                .iter()
                .find(|p| p.id == l.parent.id)
                .is_some_and(|p| p.plan_key() != l.parent.plan_key())
        })
    }

    /// `how` replayed on `parent`; `live` maps a lineage snapshot (join's right
    /// side, `:sql`'s other sheets) to the sheet as it is now.
    fn rederive(&self, parent: &Sheet, live: &dyn Fn(&Sheet) -> Sheet) -> Result<Sheet, String> {
        let Some(l) = &self.state.lineage else {
            return Err("not a derived sheet".into());
        };
        let id = self.id;
        Ok(match &l.how {
            Derivation::Copy => Sheet::copy_from(parent, id),
            Derivation::Freq { column } => Sheet::freq_from(parent, id, column),
            Derivation::Rids(rids) => {
                let df = parent
                    .view_plan_rid(true)
                    .collect()
                    .map_err(|e| e.to_string())?;
                Sheet::rows_from(parent, id, df, rids, self.name.clone())?
            }
            Derivation::Values { column, values } => {
                let values: Vec<AnyValue> = values
                    .iter()
                    .map(|v| {
                        v.clone()
                            .map_or(AnyValue::Null, |s| AnyValue::StringOwned(s.into()))
                    })
                    .collect();
                Sheet::rows_with_values(parent, id, column, &values)
            }
            Derivation::Sql { query, others } => {
                Sheet::sql_from(parent, id, query, others.iter().map(live).collect())?
            }
            Derivation::Join { right, keys, how } => {
                Sheet::join_from(parent, id, &live(right), keys, how)?
            }
            Derivation::Unpivot { on, index } => Sheet::unpivot_from(parent, id, on, index),
            Derivation::Pivot {
                index,
                on,
                values,
                agg,
            } => Sheet::pivot_from(parent, id, index, on, values, agg)?,
            Derivation::PivotCount { keys, on, aggs } => {
                Sheet::pivot_current_from(parent, id, keys, on, aggs)?
            }
            Derivation::Describe => Sheet::describe_from(parent, id)?,
            Derivation::Unfurl { column } => Sheet::unfurl_from(parent, id, column)?,
            Derivation::Diff { other, keys } => {
                Sheet::row_diff_from(parent, id, &live(other), keys)?
            }
        })
    }

    /// `fresh`'s rows under this sheet's own view (sort, hidden, keys … of
    /// columns still there) and edits; `U` goes back.
    // ponytail: undo restores plan/view/steps, not the lineage, so `:export`
    // after `U` describes the refreshed derivation.
    fn adopt(&mut self, mut fresh: Sheet) {
        self.checkpoint();
        let names: std::collections::BTreeSet<String> = fresh
            .plan
            .collect_schema()
            .map(|s| s.iter_names().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        let v = &mut self.view;
        v.hidden.retain(|c| names.contains(c));
        v.sort.retain(|k| names.contains(&k.column));
        v.keys.retain(|c| names.contains(c));
        v.type_casts.retain(|(c, _)| names.contains(c));
        v.aggregators.retain(|(c, _)| names.contains(c));
        self.plan = fresh.plan;
        self.state.lineage = fresh.state.lineage;
        self.invalidate(Reason::Plan);
    }
}

/// Refresh sheet `id` of `stack`: its derived ancestors whose parent changed
/// go first, parents before children. The sheets returned replace theirs.
pub fn refresh(id: SheetId, stack: &[Sheet]) -> Result<Refreshed, String> {
    fn current(
        s: &Sheet,
        force: bool,
        stack: &[Sheet],
        out: &mut Refreshed,
    ) -> Result<Sheet, String> {
        let Some(l) = &s.state.lineage else {
            return match force {
                true => Err("not a derived sheet".into()),
                false => Ok(s.clone()),
            };
        };
        let (parent, closed) = match stack.iter().find(|p| p.id == l.parent.id) {
            Some(p) => (current(p, false, stack, out)?, false),
            None => (l.parent.clone(), true),
        };
        if !force && parent.plan_key() == l.parent.plan_key() {
            return Ok(s.clone());
        }
        out.snapshot |= closed;
        let live = |snap: &Sheet| {
            out.sheets
                .iter()
                .chain(stack)
                .find(|x| x.id == snap.id)
                .unwrap_or(snap)
                .clone()
        };
        let fresh = s.rederive(&parent, &live)?;
        let mut s = s.clone();
        s.adopt(fresh);
        out.sheets.push(s.clone());
        Ok(s)
    }
    let s = stack.iter().find(|s| s.id == id).ok_or("no such sheet")?;
    let mut out = Refreshed {
        sheets: Vec::new(),
        snapshot: false,
    };
    current(s, true, stack, &mut out)?;
    Ok(out)
}
