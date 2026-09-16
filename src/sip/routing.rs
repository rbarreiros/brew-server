//! Voice routing engine. Given an incoming call's origin and dialled
//! destination, it consults the configured route table (top-to-bottom, first
//! match wins) and yields the resolved destination endpoint. Routes bridge SIP
//! extensions, SIP trunks and Brew (TETRA) endpoints in any combination.

use crate::config::{RouteEndpoint, VoiceRouteConfig};
use crate::sip::state::LegEndpoint;

/// The origin of a call, used to match a route's optional `from` restriction.
#[derive(Debug, Clone)]
pub enum CallOrigin {
    SipExtension(String),
    SipTrunk(String),
    BrewPrivate(u32),
    BrewGroup(u32),
}

impl CallOrigin {
    fn matches(&self, ep: &RouteEndpoint) -> bool {
        match (self, ep) {
            (CallOrigin::SipExtension(a), RouteEndpoint::SipExtension { user }) => a == user,
            (CallOrigin::SipTrunk(a), RouteEndpoint::SipTrunk { trunk, .. }) => a == trunk,
            (CallOrigin::BrewPrivate(a), RouteEndpoint::BrewPrivate { issi }) => a == issi,
            (CallOrigin::BrewGroup(a), RouteEndpoint::BrewGroup { gssi }) => a == gssi,
            _ => false,
        }
    }

    pub fn to_leg(&self) -> LegEndpoint {
        match self {
            CallOrigin::SipExtension(a) => LegEndpoint::SipExtension { aor: a.clone() },
            CallOrigin::SipTrunk(t) => LegEndpoint::SipTrunk { trunk: t.clone(), number: String::new() },
            CallOrigin::BrewPrivate(i) => LegEndpoint::BrewPrivate { issi: *i },
            CallOrigin::BrewGroup(g) => LegEndpoint::BrewGroup { gssi: *g },
        }
    }
}

/// Matches a dialled destination string against a route pattern.
/// - `*` matches anything
/// - trailing `*` (e.g. `555*`) is a prefix match
/// - otherwise exact match
fn pattern_matches(pattern: &str, dialled: &str) -> bool {
    if pattern == "*" { return true; }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return dialled.starts_with(prefix);
    }
    pattern == dialled
}

/// Converts a config endpoint to a runtime leg endpoint, filling the dialled
/// number for a trunk destination when the route did not fix one.
fn resolve_endpoint(ep: &RouteEndpoint, dialled: &str) -> LegEndpoint {
    match ep {
        RouteEndpoint::SipExtension { user } => LegEndpoint::SipExtension { aor: user.clone() },
        RouteEndpoint::SipTrunk { trunk, number } => LegEndpoint::SipTrunk {
            trunk: trunk.clone(),
            number: if number.is_empty() { dialled.to_string() } else { number.clone() },
        },
        RouteEndpoint::BrewPrivate { issi } => LegEndpoint::BrewPrivate { issi: *issi },
        RouteEndpoint::BrewGroup { gssi } => LegEndpoint::BrewGroup { gssi: *gssi },
    }
}

/// Resolves a call to its destination leg using the route table. Returns None
/// if no route matches (caller should reject the call, e.g. 404).
pub fn resolve<'a>(
    routes: &'a [VoiceRouteConfig],
    origin: &CallOrigin,
    dialled: &str,
) -> Option<(LegEndpoint, &'a VoiceRouteConfig)> {
    for route in routes {
        if !route.enabled { continue; }
        if !pattern_matches(&route.match_pattern, dialled) { continue; }
        if let Some(from) = &route.from {
            if !origin.matches(from) { continue; }
        }
        let Some(to) = &route.to else { continue };
        return Some((resolve_endpoint(to, dialled), route));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteEndpoint;

    fn route(name: &str, pat: &str, to: RouteEndpoint, from: Option<RouteEndpoint>) -> VoiceRouteConfig {
        VoiceRouteConfig {
            name: name.into(),
            match_pattern: pat.into(),
            to: Some(to),
            from,
            enabled: true,
        }
    }

    #[test]
    fn prefix_and_exact_matching() {
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("555*", "5551234"));
        assert!(!pattern_matches("555*", "6001234"));
        assert!(pattern_matches("2002", "2002"));
        assert!(!pattern_matches("2002", "2003"));
    }

    #[test]
    fn first_matching_route_wins() {
        let routes = vec![
            route("to-trunk", "9*", RouteEndpoint::SipTrunk { trunk: "asterisk".into(), number: String::new() }, None),
            route("to-ext", "*", RouteEndpoint::SipExtension { user: "operator".into() }, None),
        ];
        let origin = CallOrigin::SipExtension("1001".into());
        let (leg, r) = resolve(&routes, &origin, "95551234").unwrap();
        assert_eq!(r.name, "to-trunk");
        match leg {
            LegEndpoint::SipTrunk { trunk, number } => { assert_eq!(trunk, "asterisk"); assert_eq!(number, "95551234"); }
            _ => panic!("expected trunk leg"),
        }
    }

    #[test]
    fn from_restriction_is_honoured() {
        let routes = vec![
            route("trunk-in-to-group", "*",
                RouteEndpoint::BrewGroup { gssi: 1001 },
                Some(RouteEndpoint::SipTrunk { trunk: "asterisk".into(), number: String::new() })),
        ];
        // A call from the extension does NOT match the trunk-only route.
        assert!(resolve(&routes, &CallOrigin::SipExtension("1001".into()), "anything").is_none());
        // A call from the trunk does.
        let (leg, _) = resolve(&routes, &CallOrigin::SipTrunk("asterisk".into()), "anything").unwrap();
        matches!(leg, LegEndpoint::BrewGroup { gssi: 1001 });
    }

    #[test]
    fn extension_to_brew_private() {
        let routes = vec![
            route("ext-to-issi", "7*", RouteEndpoint::BrewPrivate { issi: 90 }, None),
        ];
        let (leg, _) = resolve(&routes, &CallOrigin::SipExtension("1001".into()), "790").unwrap();
        match leg {
            LegEndpoint::BrewPrivate { issi } => assert_eq!(issi, 90),
            _ => panic!("expected brew private leg"),
        }
    }

    #[test]
    fn no_route_returns_none() {
        let routes = vec![route("only-9", "9*", RouteEndpoint::SipTrunk { trunk: "t".into(), number: String::new() }, None)];
        assert!(resolve(&routes, &CallOrigin::SipExtension("1001".into()), "1234").is_none());
    }
}
