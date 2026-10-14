use sitty_proto::{Direction, Layout};

/// Replace leaf `of` with a split of [of, new]. Returns false if `of` is not in the tree.
pub fn split(layout: &mut Layout, of: &str, new: &str, dir: Direction) -> bool {
    match layout {
        Layout::Pane { id } if id == of => {
            let old = Layout::Pane { id: id.clone() };
            *layout = Layout::Split { direction: dir, children: vec![old, Layout::Pane { id: new.into() }] };
            true
        }
        Layout::Pane { .. } => false,
        Layout::Split { children, .. } => children.iter_mut().any(|c| split(c, of, new, dir)),
    }
}

/// Remove leaf `id`. Returns None when the tree becomes empty; collapses one-child splits.
pub fn remove(layout: Layout, id: &str) -> Option<Layout> {
    match layout {
        Layout::Pane { id: ref leaf } if leaf == id => None,
        Layout::Pane { .. } => Some(layout),
        Layout::Split { direction, children } => {
            let mut kept: Vec<Layout> = children.into_iter().filter_map(|c| remove(c, id)).collect();
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(Layout::Split { direction, children: kept }),
            }
        }
    }
}

pub fn leaves(layout: &Layout) -> Vec<String> {
    match layout {
        Layout::Pane { id } => vec![id.clone()],
        Layout::Split { children, .. } => children.iter().flat_map(leaves).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str) -> Layout {
        Layout::Pane { id: id.into() }
    }

    #[test]
    fn split_a_leaf() {
        let mut l = p("p1");
        assert!(split(&mut l, "p1", "p2", Direction::Right));
        assert_eq!(l, Layout::Split { direction: Direction::Right, children: vec![p("p1"), p("p2")] });
    }

    #[test]
    fn split_nested_leaf_inserts_next_to_it() {
        let mut l = Layout::Split { direction: Direction::Right, children: vec![p("p1"), p("p2")] };
        assert!(split(&mut l, "p2", "p3", Direction::Down));
        assert_eq!(leaves(&l), vec!["p1", "p2", "p3"]);
        assert!(!split(&mut l, "nope", "p4", Direction::Down));
    }

    #[test]
    fn remove_collapses_single_child_splits() {
        let l = Layout::Split { direction: Direction::Right, children: vec![p("p1"), p("p2")] };
        assert_eq!(remove(l, "p2"), Some(p("p1")));
        assert_eq!(remove(p("p1"), "p1"), None);
    }
}
