//! Semantic (order-insensitive on sibling elements) XML comparison for parity tests.

use std::collections::BTreeMap;

use super::dom::{XmlDocument, XmlElement, XmlNode};

/// Returns `true` if two XML documents are structurally equivalent regardless of sibling
/// element ordering or attribute ordering, matching SDR's `compareTwoXml` test helper.
pub fn semantic_eq(left: &XmlDocument, right: &XmlDocument) -> bool {
    element_eq(&left.root, &right.root)
}

fn element_eq(left: &XmlElement, right: &XmlElement) -> bool {
    if left.name != right.name {
        return false;
    }

    let mut left_attrs: Vec<(&str, &str)> = left
        .attributes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut right_attrs: Vec<(&str, &str)> = right
        .attributes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    left_attrs.sort_unstable();
    right_attrs.sort_unstable();
    if left_attrs != right_attrs {
        return false;
    }

    let left_text = collect_non_element_nodes(&left.children);
    let right_text = collect_non_element_nodes(&right.children);
    if left_text != right_text {
        return false;
    }

    let left_groups = group_child_elements(&left.children);
    let right_groups = group_child_elements(&right.children);
    if left_groups.len() != right_groups.len() {
        return false;
    }

    for (tag, left_elems) in left_groups {
        let Some(right_elems) = right_groups.get(tag) else {
            return false;
        };
        if left_elems.len() != right_elems.len() {
            return false;
        }
        let mut matched = vec![false; right_elems.len()];
        for le in left_elems {
            let Some(pos) = right_elems
                .iter()
                .enumerate()
                .position(|(idx, re)| !matched[idx] && element_eq(le, re))
            else {
                return false;
            };
            matched[pos] = true;
        }
    }

    true
}

fn collect_non_element_nodes(children: &[XmlNode]) -> Vec<(u8, &str)> {
    children
        .iter()
        .filter_map(|n| match n {
            XmlNode::Text(s) => Some((0, s.as_str())),
            XmlNode::CData(s) => Some((1, s.as_str())),
            XmlNode::Comment(s) => Some((2, s.as_str())),
            XmlNode::Element(_) => None,
        })
        .collect()
}

fn group_child_elements(children: &[XmlNode]) -> BTreeMap<&str, Vec<&XmlElement>> {
    let mut map: BTreeMap<&str, Vec<&XmlElement>> = BTreeMap::new();
    for node in children {
        if let XmlNode::Element(el) = node {
            map.entry(el.name.as_str()).or_default().push(el);
        }
    }
    map
}
