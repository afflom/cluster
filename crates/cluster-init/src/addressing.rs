//! Turning an ordinal into an address (`SPEC.md` §4.1).
//!
//! This is the same arithmetic `cluster-model` renders the firewall from, and it
//! is here because the node has to compute it too --- for a link whose peer it
//! discovered rather than one it was told about. Both implementations read the
//! same two bases out of the same rendered file, so neither carries a number of
//! its own; what they share is the *policy*, not a hard-coded answer.
//!
//! The property everything rests on: **both ends of a cable derive the same
//! link from the same two ordinals, in either order, with nothing exchanged but
//! the ordinals.** The lower ordinal takes the even address of the `/31`, which
//! is what makes the two ends agree without negotiating.

use std::net::Ipv4Addr;

use crate::InitError;

/// The bases and prefix lengths, read from the rendered policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addressing {
    /// A node's loopback is this plus its ordinal.
    pub loopback_base: Ipv4Addr,
    /// `32`.
    pub loopback_prefix_len: u8,
    /// A link's prefix is this plus twice its index.
    pub link_base: Ipv4Addr,
    /// `31`, per RFC 3021.
    pub link_prefix_len: u8,
    /// How many ordinals exist.
    pub fleet_size: u32,
}

/// A point-to-point link between two ordinals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// The lower ordinal, which takes the even address.
    pub lower: u32,
    /// The higher ordinal.
    pub higher: u32,
    /// The even address.
    pub lower_address: Ipv4Addr,
    /// The odd address.
    pub higher_address: Ipv4Addr,
    /// `31`.
    pub prefix_len: u8,
}

impl Link {
    /// `ordinal`'s own address on this link.
    pub fn address_of(&self, ordinal: u32) -> Option<Ipv4Addr> {
        if self.lower == ordinal {
            Some(self.lower_address)
        } else if self.higher == ordinal {
            Some(self.higher_address)
        } else {
            None
        }
    }

    /// The ordinal on the other end.
    pub fn peer_of(&self, ordinal: u32) -> Option<u32> {
        if self.lower == ordinal {
            Some(self.higher)
        } else if self.higher == ordinal {
            Some(self.lower)
        } else {
            None
        }
    }
}

impl Addressing {
    /// The loopback of an ordinal.
    pub fn loopback_of(&self, ordinal: u32) -> Result<Ipv4Addr, InitError> {
        self.check(ordinal)?;
        Ok(Ipv4Addr::from(
            u32::from(self.loopback_base) + ordinal,
        ))
    }

    /// The link joining two ordinals, in either order.
    pub fn link_between(&self, x: u32, y: u32) -> Result<Link, InitError> {
        self.check(x)?;
        self.check(y)?;
        if x == y {
            return Err(InitError::Addressing(format!(
                "ordinal {x} has no link to itself"
            )));
        }
        let (lower, higher) = if x < y { (x, y) } else { (y, x) };
        let index = self.pair_index(lower, higher).ok_or_else(|| {
            InitError::Addressing(format!("({lower},{higher}) is not a pair in this fleet"))
        })?;
        let prefix = u32::from(self.link_base) + 2 * index;
        Ok(Link {
            lower,
            higher,
            lower_address: Ipv4Addr::from(prefix),
            higher_address: Ipv4Addr::from(prefix + 1),
            prefix_len: self.link_prefix_len,
        })
    }

    fn check(&self, ordinal: u32) -> Result<(), InitError> {
        if ordinal == 0 || ordinal > self.fleet_size {
            return Err(InitError::Addressing(format!(
                "ordinal {ordinal} is outside a fleet of {}",
                self.fleet_size
            )));
        }
        Ok(())
    }

    /// The position of an unordered pair among all pairs, ascending.
    fn pair_index(&self, lower: u32, higher: u32) -> Option<u32> {
        let mut index = 0;
        for a in 1..=self.fleet_size {
            for b in (a + 1)..=self.fleet_size {
                if a == lower && b == higher {
                    return Some(index);
                }
                index += 1;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addressing() -> Addressing {
        Addressing {
            loopback_base: "10.10.255.0".parse().unwrap(),
            loopback_prefix_len: 32,
            link_base: "10.10.0.0".parse().unwrap(),
            link_prefix_len: 31,
            fleet_size: 3,
        }
    }

    /// §4.1's table, derived rather than read.
    #[test]
    fn the_derivation_reproduces_the_specified_table() {
        let a = addressing();
        assert_eq!(a.loopback_of(1).unwrap().to_string(), "10.10.255.1");
        assert_eq!(a.loopback_of(3).unwrap().to_string(), "10.10.255.3");
        assert_eq!(
            a.link_between(1, 2).unwrap().lower_address.to_string(),
            "10.10.0.0"
        );
        assert_eq!(
            a.link_between(1, 3).unwrap().lower_address.to_string(),
            "10.10.0.2"
        );
        assert_eq!(
            a.link_between(2, 3).unwrap().lower_address.to_string(),
            "10.10.0.4"
        );
    }

    /// The property the whole scheme rests on. A node discovers a peer's
    /// ordinal across a cable and addresses that cable with no further exchange;
    /// this is why that works.
    #[test]
    fn both_ends_agree_with_nothing_exchanged_but_ordinals() {
        let a = addressing();
        for (x, y) in [(1, 2), (1, 3), (2, 3)] {
            let mine = a.link_between(x, y).unwrap();
            let theirs = a.link_between(y, x).unwrap();
            assert_eq!(mine, theirs);
            assert_eq!(mine.address_of(x), theirs.address_of(x));
            assert_ne!(mine.address_of(x), mine.address_of(y));
        }
    }

    #[test]
    fn the_lower_ordinal_takes_the_even_address() {
        let a = addressing();
        for (x, y) in [(1, 2), (1, 3), (2, 3)] {
            let link = a.link_between(x, y).unwrap();
            assert!(u32::from(link.lower_address).is_multiple_of(2));
            assert_eq!(
                u32::from(link.higher_address),
                u32::from(link.lower_address) + 1
            );
        }
    }

    /// This crate does not invent an ordinal it was not given. An ordinal
    /// outside the fleet is a registrar that answered wrongly, and a node that
    /// addressed itself from it would collide with something.
    #[test]
    fn an_ordinal_outside_the_fleet_is_refused() {
        let a = addressing();
        assert!(a.loopback_of(0).is_err());
        assert!(a.loopback_of(4).is_err());
        assert!(a.link_between(1, 4).is_err());
        assert!(a.link_between(2, 2).is_err());
    }
}
