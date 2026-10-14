//! Explicit context selects verified activity; it never authorizes a URL fetch or a write.
use crate::ado::link::Linkable;
use std::collections::BTreeSet;
use url::Url;

#[derive(Default)]
pub(crate) struct ExplicitContext {
    items: Vec<u64>,
    urls: Vec<Url>,
}
impl ExplicitContext {
    pub(crate) fn new(text: &str) -> Self {
        let urls = regex::Regex::new(r#"https://[^\s<>\"']+"#)
            .unwrap()
            .find_iter(text)
            .filter_map(|m| {
                Url::parse(m.as_str().trim_end_matches(['.', ',', ';', '!', ')', ']'])).ok()
            })
            .filter(|url| {
                url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
            })
            .collect();
        Self {
            items: crate::ado::review::mentions(text),
            urls,
        }
    }
    pub(crate) fn work_items(&self, organization: &str) -> BTreeSet<u64> {
        self.items
            .iter()
            .copied()
            .chain(
                self.urls
                    .iter()
                    .filter(|u| in_organization(u, organization))
                    .filter_map(work_item),
            )
            .filter(|id| *id > 0)
            .take(20)
            .collect()
    }
    pub(crate) fn mentions(&self, activity: &Linkable) -> bool {
        if activity
            .related_work_items
            .iter()
            .any(|id| self.items.contains(id))
        {
            return true;
        }
        std::iter::once(&activity.url)
            .chain(&activity.aliases)
            .any(|verified| {
                let Ok(verified) = Url::parse(verified) else {
                    return false;
                };
                self.urls.iter().any(|u| {
                    in_organization(u, &activity.organization)
                        && (same_artifact(u, &verified)
                            || (same_scope(u, &verified)
                                && work_item(u)
                                    .is_some_and(|id| activity.related_work_items.contains(&id))))
                })
            })
    }
}
fn in_organization(url: &Url, organization: &str) -> bool {
    let Ok(org) = Url::parse(organization) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == org.host_str()
        && url.port_or_known_default() == org.port_or_known_default()
        && (org.path().trim_matches('/').is_empty()
            || url.path_segments().and_then(|mut p| p.next())
                == org.path_segments().and_then(|mut p| p.next()))
}
fn work_item(url: &Url) -> Option<u64> {
    let parts: Vec<_> = url.path_segments()?.collect();
    if parts.get(2) == Some(&"_workitems") && parts.get(3) == Some(&"edit") {
        parts.get(4)?.parse().ok()
    } else if parts.get(2) == Some(&"_sprints") {
        url.query_pairs()
            .find(|(k, _)| k.eq_ignore_ascii_case("workitem"))?
            .1
            .parse()
            .ok()
    } else {
        None
    }
}
fn same_scope(left: &Url, right: &Url) -> bool {
    left.origin() == right.origin()
        && left.path_segments().map(|p| p.take(2).collect::<Vec<_>>())
            == right.path_segments().map(|p| p.take(2).collect::<Vec<_>>())
}
fn same_artifact(left: &Url, right: &Url) -> bool {
    if !same_scope(left, right) {
        return false;
    }
    let l: Vec<_> = left.path_segments().into_iter().flatten().collect();
    let r: Vec<_> = right.path_segments().into_iter().flatten().collect();
    if l.get(..2) != r.get(..2) {
        return false;
    }
    let query = |u: &Url, name: &str| {
        u.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    for (route, parameter) in [("_releaseProgress", "releaseId"), ("_build", "buildId")] {
        if l.get(2) == Some(&route) && r.get(2) == Some(&route) {
            return query(left, parameter).is_some()
                && query(left, parameter) == query(right, parameter);
        }
    }
    if l.get(2..4) == Some(&["_wiki", "wikis"])
        && r.get(2..4) == Some(&["_wiki", "wikis"])
        && l.get(4) == r.get(4)
        && let (Some(id), Some(other)) = (l.get(5), r.get(5))
    {
        return id.parse::<u64>().is_ok() && id == other;
    }
    let mut l = left.clone();
    let mut r = right.clone();
    l.set_fragment(None);
    r.set_fragment(None);
    let canonical = |url: &mut Url| {
        let mut pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();
        url.set_query(None);
        url.query_pairs_mut().extend_pairs(pairs);
    };
    canonical(&mut l);
    canonical(&mut r);
    l == r
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_matches_verified_routes_and_associations_without_crossing_organizations() {
        let mut activity = Linkable {
            organization: "https://dev.azure.com/example".into(),
            url: "https://dev.azure.com/example/Payments/_releaseProgress?_a=release-pipeline-progress&releaseId=2760".into(),
            related_work_items: vec![24663], ..Default::default()
        };
        for text in [
            "Released https://dev.azure.com/example/Payments/_releaseProgress?releaseId=2760&_a=other.",
            "Worked on https://dev.azure.com/example/Payments/_sprints/taskboard/team?workitem=24663",
            "Worked on HU 24663",
        ] {
            assert!(ExplicitContext::new(text).mentions(&activity), "{text}");
        }
        assert!(
            !ExplicitContext::new(
                "https://dev.azure.com/other/Payments/_releaseProgress?releaseId=2760"
            )
            .mentions(&activity)
        );
        assert!(
            !ExplicitContext::new(
                "https://dev.azure.com/example/Other/_releaseProgress?releaseId=2760"
            )
            .mentions(&activity)
        );
        assert!(
            !ExplicitContext::new(
                "https://dev.azure.com/other/Payments/_sprints/taskboard/team?workitem=24663"
            )
            .mentions(&activity)
        );
        assert!(
            !ExplicitContext::new(
                "https://dev.azure.com/example/Other/_sprints/taskboard/team?workitem=24663"
            )
            .mentions(&activity)
        );
        activity.related_work_items.clear();
        activity.aliases =
            vec!["https://dev.azure.com/example/Payments/_wiki/wikis/docs/6930".into()];
        assert!(
            ExplicitContext::new(
                "Edited https://dev.azure.com/example/Payments/_wiki/wikis/docs/6930/Page-title"
            )
            .mentions(&activity)
        );
        assert!(
            !ExplicitContext::new(
                "Edited https://dev.azure.com/example/Payments/_wiki/wikis/docs/6931/Page-title"
            )
            .mentions(&activity)
        );
        activity.organization = "https://teams.microsoft.com".into();
        activity.url = "https://teams.microsoft.com/l/message/own-chat/123?tenantId=example".into();
        assert!(ExplicitContext::new(&activity.url).mentions(&activity));
        assert!(
            !ExplicitContext::new(
                "https://teams.microsoft.com/l/message/another-chat/123?tenantId=example"
            )
            .mentions(&activity)
        );
    }
}
