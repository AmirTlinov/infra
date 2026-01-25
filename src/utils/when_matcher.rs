use crate::utils::fs_atomic::path_exists;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn file_exists(root: Option<&Path>, candidate: &str, cache: &mut HashMap<String, bool>) -> bool {
    if root.is_none() || candidate.trim().is_empty() {
        return false;
    }
    if let Some(hit) = cache.get(candidate) {
        return *hit;
    }
    let root = root.unwrap();
    let full = if Path::new(candidate).is_absolute() {
        PathBuf::from(candidate)
    } else {
        root.join(candidate)
    };
    let exists = path_exists(full);
    cache.insert(candidate.to_string(), exists);
    exists
}

pub fn match_tags(tags: &[String], context_tags: &[String]) -> Vec<String> {
    let tag_set: HashSet<&String> = context_tags.iter().collect();
    tags.iter()
        .filter(|tag| tag_set.contains(tag))
        .cloned()
        .collect()
}

pub fn matches_when(when: &Value, context: &Value) -> bool {
    if when.is_null() {
        return true;
    }
    let Some(obj) = when.as_object() else {
        return false;
    };

    let tags: HashSet<String> = context
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut files_cache: HashMap<String, bool> = context
        .get("files")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
                .collect()
        })
        .unwrap_or_default();

    let root = context
        .get("root")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    if let Some(all_of) = obj.get("all_of").and_then(|v| v.as_array()) {
        for entry in all_of {
            if !matches_when(entry, context) {
                return false;
            }
        }
    }

    if let Some(any_of) = obj.get("any_of").and_then(|v| v.as_array()) {
        let mut hit = false;
        for entry in any_of {
            if matches_when(entry, context) {
                hit = true;
                break;
            }
        }
        if !hit {
            return false;
        }
    }

    if let Some(not_val) = obj.get("not") {
        if matches_when(not_val, context) {
            return false;
        }
    }

    if let Some(tags_any) = obj.get("tags_any").and_then(|v| v.as_array()) {
        if !tags_any
            .iter()
            .filter_map(|v| v.as_str())
            .any(|tag| tags.contains(tag))
        {
            return false;
        }
    }

    if let Some(tags_all) = obj.get("tags_all").and_then(|v| v.as_array()) {
        if !tags_all
            .iter()
            .filter_map(|v| v.as_str())
            .all(|tag| tags.contains(tag))
        {
            return false;
        }
    }

    if let Some(files_any) = obj.get("files_any").and_then(|v| v.as_array()) {
        let mut hit = false;
        for entry in files_any.iter().filter_map(|v| v.as_str()) {
            if file_exists(root.as_deref(), entry, &mut files_cache) {
                hit = true;
                break;
            }
        }
        if !hit {
            return false;
        }
    }

    if let Some(files_all) = obj.get("files_all").and_then(|v| v.as_array()) {
        for entry in files_all.iter().filter_map(|v| v.as_str()) {
            if !file_exists(root.as_deref(), entry, &mut files_cache) {
                return false;
            }
        }
    }

    true
}
