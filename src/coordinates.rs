use crate::frames::TreeAnnouncement;
use crate::router::Port;

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Coordinates {
    pub(crate) coordinates: Vec<Port>,
}

impl Coordinates {
    pub(crate) fn distance_to(&self, to: &Coordinates) -> usize {
        self.coordinates.len() + to.coordinates.len() - 2 * self.get_common_prefix(&to)
    }

    fn get_common_prefix(&self, to: &Coordinates) -> usize {
        let mut c: usize = 0;
        let mut l: usize = self.coordinates.len();
        if to.coordinates.len() < l {
            l = to.coordinates.len();
        }
        for i in 0..l {
            if self.coordinates[i] != to.coordinates[i] {
                break;
            }
            c += 1;
        }
        c
    }

    pub(crate) fn new(coordinates: Vec<Port>) -> Self {
        Coordinates { coordinates }
    }
}

impl Default for Coordinates {
    fn default() -> Self {
        Coordinates {
            coordinates: vec![],
        }
    }
}
impl TreeAnnouncement {
    pub(crate) fn coords(&self) -> Coordinates {
        let mut coordinates = Coordinates::default();
        for signature in &self.signatures {
            coordinates.coordinates.push(signature.destination_port);
        }
        coordinates
    }
    pub(crate) fn peer_coords(&self) -> Coordinates {
        let mut coordinates = Coordinates::default();
        for i in 0..self.signatures.len() - 1 {
            coordinates
                .coordinates
                .push(self.signatures.get(i).unwrap().destination_port);
        }
        coordinates
    }
}
