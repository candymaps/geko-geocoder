/// Union-find with path halving and union by size.
pub struct UnionFind {
    parent: Vec<u32>,
    size: Vec<u32>,
}

impl UnionFind {
    pub fn new(n: usize) -> Self {
        Self { parent: (0..n as u32).collect(), size: vec![1; n] }
    }

    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let grand = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = grand;
            x = grand;
        }
        x
    }

    pub fn union(&mut self, a: u32, b: u32) {
        let (mut a, mut b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        if self.size[a as usize] < self.size[b as usize] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b as usize] = a;
        self.size[a as usize] += self.size[b as usize];
    }

    /// Groups of member indices, each sorted, in order of their smallest member.
    pub fn groups(mut self) -> Vec<Vec<u32>> {
        let n = self.parent.len();
        let mut by_root: rustc_hash::FxHashMap<u32, Vec<u32>> = rustc_hash::FxHashMap::default();
        for i in 0..n as u32 {
            let root = self.find(i);
            by_root.entry(root).or_default().push(i);
        }
        let mut groups: Vec<Vec<u32>> = by_root.into_values().collect();
        groups.sort_by_key(|g| g[0]);
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_connected_members() {
        let mut uf = UnionFind::new(6);
        uf.union(0, 2);
        uf.union(4, 2);
        uf.union(1, 5);
        assert_eq!(uf.groups(), vec![vec![0, 2, 4], vec![1, 5], vec![3]]);
    }
}
