use crate::lazy_tree::LazyTree;
use ic_crypto_tree_hash::{
    self as crypto, hasher::Hasher, Digest, Label, LabeledTree, WitnessBuilder,
};
use itertools::izip;
use std::collections::VecDeque;
use std::fmt;
use std::iter::repeat_with;
use std::ops::Range;

/// The number of threads we use for building HashTree
pub const NUMBER_OF_CERTIFICATION_THREADS: u32 = 16;

const EMPTY_HASH: Digest = Digest([
    0x4e, 0x3e, 0xd3, 0x5c, 0x4e, 0x2d, 0x1e, 0xe8, 0x99, 0x96, 0x48, 0x3f, 0xb6, 0x26, 0x0a, 0x64,
    0xcf, 0xfb, 0x6c, 0x47, 0xdb, 0xab, 0x21, 0x6e, 0x79, 0x30, 0xe8, 0x2f, 0x81, 0x90, 0xd1, 0x20,
]);

const INDEX_MASK: u32 = 0x3fff_ffff;
const KIND_MASK: u32 = 0xc000_0000;
const LEAF_KIND: u32 = 0x4000_0000;
const NODE_KIND: u32 = 0x8000_0000;
const FORK_KIND: u32 = 0xc000_0000;

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum NodeKind {
    Empty,
    Fork,
    Leaf,
    Node,
}

/// NodeId describe the position of a node in the HashTree data structure
///
/// HashTree consists of several parallel vectors of vectors. The kind of node
/// is node_id.kind(), the first index is node_id.bucket(), whereas the second
/// index is node_id.index().
///
/// For example, the digest of a node_id with node_id.kind() = NodeKind::Fork can
/// is stored at hash_tree.fork_digests[node_id.bucket()][node_id.index()]
///
/// The reason for storing vectors of vectors is because it lends itself to parallelism
/// when computing the HashTree.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Default)]
pub struct NodeId {
    bucket: u32,
    index_and_kind: u32,
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            NodeKind::Empty => write!(f, "Empty"),
            NodeKind::Fork => write!(f, "Fork({}, {})", self.bucket(), self.index()),
            NodeKind::Leaf => write!(f, "Leaf({}, {})", self.bucket(), self.index()),
            NodeKind::Node => write!(f, "Node({}, {})", self.bucket(), self.index()),
        }
    }
}

impl NodeId {
    /// Constructs an empty tree.
    #[inline]
    pub fn empty() -> Self {
        Self {
            bucket: 0,
            index_and_kind: 0,
        }
    }

    /// Constructs a new Fork with the specified index.
    #[inline]
    pub fn fork(bucket: usize, idx: usize) -> Self {
        Self {
            bucket: bucket as u32,
            index_and_kind: FORK_KIND | idx as u32,
        }
    }

    /// Constructs a new Leaf with the specified index.
    #[inline]
    pub fn leaf(bucket: usize, idx: usize) -> Self {
        Self {
            bucket: bucket as u32,
            index_and_kind: LEAF_KIND | idx as u32,
        }
    }

    /// Constructs a new Node with the specified index.
    #[inline]
    pub fn node(bucket: usize, idx: usize) -> Self {
        Self {
            bucket: bucket as u32,
            index_and_kind: NODE_KIND | idx as u32,
        }
    }

    /// Returns the component kind of this node.
    #[inline]
    pub fn kind(self) -> NodeKind {
        let node_id = self.index_and_kind;
        match node_id & KIND_MASK {
            FORK_KIND => NodeKind::Fork,
            NODE_KIND => NodeKind::Node,
            LEAF_KIND => NodeKind::Leaf,
            _ => NodeKind::Empty,
        }
    }

    /// Returns the index component of this node.
    #[inline]
    pub fn index(self) -> usize {
        (self.index_and_kind & INDEX_MASK) as usize
    }

    #[inline]
    pub fn bucket(self) -> usize {
        self.bucket as usize
    }
}

/// A range of NodeIds that share the same bucket and have consecutive indices
/// index_range is to be understood as a half-open range
#[derive(Clone, Debug, Default)]
struct NodeIndexRange {
    bucket: usize,
    index_range: Range<usize>,
}

impl NodeIndexRange {
    fn indexes_into(&self, hash_tree: &HashTree) -> bool {
        self.bucket < hash_tree.node_digests.len()
            && self.index_range.end <= hash_tree.node_digests[self.bucket].len()
    }
}

/// Compact array-based hash tree representation.
///
/// Normally we'd represent this data-structure using an enum, which works fine
/// for moderately sized trees.  This tree uses a slightly more complicated
/// representation that makes it a better choice for large trees:
///
///   * It uses about 30% less memory and has better memory locality.
///
///   * It's much faster to deallocate (some benchmarks showed 2 orders of
///     magnitude difference compared to the enum representation).
///
///   * The fact that nodes of the same type are stored consecutively allows us
///     to build witnesses without any additional data structures.
///
/// The `view` function provides a convenient interface for traversing the
/// compact tree as if it was a node-based structure.
///
/// ## Notes on the tree layout
///
/// The tree is represented as a collection of parallel arrays of arrays
/// ([structure-of-arrays][1]).  For example, a tree like
///
/// ```text
/// (fork (label "x" (leaf "data1")) (label "y" (leaf "data2")))
/// ```
///
/// might be represented as:
///
/// ```text
/// root:          fork_0
/// fork_lefts:    [[node_0]]
/// fork_rights:   [[node_1]]
///
/// node_labels:   [["x",     "y"]]
/// node_children: [[leaf_0,  leaf_1]]
///
/// leaves:        [["data1", "data2"]]
/// ```
///
/// In this representation, the identifier of a node are two 32 bit unsigned
/// integers, where the first number indexes into the (outer) vector and for
/// the second number , the 2 most significant bits are used to indicate the
/// type of the node:
///
///  * (0,00) is an empty tree.
///  * (0,01) is a leaf.
///  * (0,10) is a labeled node.
///  * (0,11) is a fork.
///
///  This means that the tree can store at most 2^30 nodes of the same type.  As
///  each tree node has a 32-byte hash associated with it, the tree needs to
///  occupy at least 32 GiB of data before the index overflows.
///
/// [1]: https://en.wikipedia.org/wiki/AoS_and_SoA
#[derive(Clone, Debug)]
pub struct HashTree {
    bucket_offset: usize,
    /// Id of the root of the tree.
    root: NodeId,
    /// If the tree root is a fork or a node, root_labels_range represents a
    /// half-closed index interval [i, j) pointing into the node_labels array
    /// containing all the labels on edges of the original tree going out of the
    /// root.
    ///
    /// INVARIANT: bucket ≤ node_labels.len()
    /// index_range.0 <= index_range.1 <= node_labels[bucket].len()
    root_labels_range: NodeIndexRange,

    /// (i,j)-th element of this array contains the hash of the leaf with id
    /// `NodeId::leaf(i,j)`.
    leaf_digests: Vec<Vec<Digest>>,

    // INVARIANT:
    // fork_digests.len() == fork_left_children.len() == fork_right_children.len().
    // forall i: fork_digest[i].len() == fork_left_children[i].len() ==
    // fork_right_children[i].len()
    /// (i,j)-th element of this array contains the hash of the fork with id equal
    /// to `NodeId::fork(i,j)`.
    fork_digests: Vec<Vec<Digest>>,
    /// (i,j)-th element of this array contains the node id of the left child of the
    /// fork with id `NodeId::fork(i,j)`.
    fork_left_children: Vec<Vec<NodeId>>,
    /// (i,j)-th element of this array contains the node id of the right child of
    /// the fork with id `NodeId::fork(i,j)`.
    fork_right_children: Vec<Vec<NodeId>>,

    // INVARIANT:
    // node_digests.len() == node_labels.len() == node_children.len() ==
    // node_children_labels_ranges.len()
    // forall i: node_digests[i].len() == node_labels[i].len() ==
    // node_children[i].len() == node_children_labels_ranges[i].len()
    //
    // INVARIANT:
    // labels having the same parent node are stored consecutively in the same bucket.
    /// (i,j)-th element of this array contains the hash of the labeled node with id
    /// `NodeId::node(i,j)`.
    node_digests: Vec<Vec<Digest>>,
    /// (i,j)-th element of this array contains the label of the labeled node with
    /// id `NodeId::node(i,j)`.
    node_labels: Vec<Vec<Label>>,
    /// (i,j)-th element of this array contains the direct child of the labeled node
    /// with id `NodeId::node(i,j)`.
    node_children: Vec<Vec<NodeId>>,
    /// (i,j)-th element of this array contains an IndexRange pointing to a
    /// half-closed index interval [a, b) in of of the buckets
    /// pointing into the node_labels array containing all the labels on edges
    /// of the original tree going out of the node with id `NodeId::node(i,j)`.
    ///
    /// INVARIANT: bucket ≤ node_labels.len()
    /// index_range.0 <= index_range.1 <= node_labels[bucket].len()
    node_children_labels_ranges: Vec<Vec<NodeIndexRange>>,
}

impl HashTree {
    fn new() -> Self {
        Self::new_with_bucket_offset(0)
    }

    fn new_with_bucket_offset(bucket_offset: usize) -> Self {
        Self {
            bucket_offset,
            root: Default::default(),
            root_labels_range: Default::default(),
            leaf_digests: vec![Default::default()],
            fork_digests: vec![Default::default()],
            fork_left_children: vec![Default::default()],
            fork_right_children: vec![Default::default()],
            node_digests: vec![Default::default()],
            node_labels: vec![Default::default()],
            node_children: vec![Default::default()],
            node_children_labels_ranges: vec![Default::default()],
        }
    }

    // Note that new forks are always added to fork_digests[0], but in order
    // to access it, you use a NodeId with bucket set to self.bucket_offset.
    fn new_fork(&mut self, d: Digest, l: NodeId, r: NodeId) -> NodeId {
        let id = self.fork_digests[0].len();

        self.fork_digests[0].push(d);
        self.fork_left_children[0].push(l);
        self.fork_right_children[0].push(r);

        NodeId::fork(self.bucket_offset, id)
    }

    fn reserve_forks(&mut self, additional: usize) {
        self.fork_digests[0].reserve(additional);
        self.fork_left_children[0].reserve(additional);
        self.fork_right_children[0].reserve(additional);
    }

    fn new_leaf(&mut self, d: Digest) -> NodeId {
        let id = self.leaf_digests[0].len();
        self.leaf_digests[0].push(d);
        NodeId::leaf(self.bucket_offset, id)
    }

    fn preallocate_nodes(&mut self, len: usize, parent: NodeId) -> NodeIndexRange {
        if parent != NodeId::empty() {
            debug_assert_eq!(parent.bucket(), self.bucket_offset);
        }
        let old_len = self.node_labels[0].len();
        let new_len = old_len + len;

        self.node_labels[0].resize(new_len, Default::default());
        self.node_digests[0].resize(new_len, Digest([0; 32]));
        self.node_children[0].resize(new_len, NodeId::empty());
        self.node_children_labels_ranges[0].resize(new_len, Default::default());

        let range = NodeIndexRange {
            bucket: self.bucket_offset,
            index_range: old_len..new_len,
        };

        if parent == NodeId::empty() {
            self.root_labels_range = range.clone()
        } else {
            debug_assert_eq!(NodeKind::Node, parent.kind());
            self.node_children_labels_ranges[0][parent.index()] = range.clone()
        }
        range
    }

    fn node_labels_range(&self, parent: NodeId) -> NodeIndexRange {
        if parent == NodeId::empty() {
            self.root_labels_range.clone()
        } else {
            self.node_children_labels_ranges[parent.bucket() - self.bucket_offset][parent.index()]
                .clone()
        }
    }

    fn digest(&self, node_id: NodeId) -> &Digest {
        match node_id.kind() {
            NodeKind::Fork => {
                &self.fork_digests[node_id.bucket() - self.bucket_offset][node_id.index()]
            }
            NodeKind::Node => {
                &self.node_digests[node_id.bucket() - self.bucket_offset][node_id.index()]
            }
            NodeKind::Leaf => {
                &self.leaf_digests[node_id.bucket() - self.bucket_offset][node_id.index()]
            }
            NodeKind::Empty => &EMPTY_HASH,
        }
    }

    fn check_invariants(&self) {
        fn check_same_dimensions<S, T>(l: &Vec<Vec<S>>, r: &Vec<Vec<T>>) {
            debug_assert_eq!(l.len(), r.len());
            debug_assert!(l.iter().zip(r.iter()).all(|(l, r)| l.len() == r.len()));
        }

        debug_assert!(self.root_labels_range.indexes_into(self));

        check_same_dimensions(&self.fork_digests, &self.fork_left_children);
        check_same_dimensions(&self.fork_digests, &self.fork_right_children);

        check_same_dimensions(&self.node_digests, &self.node_labels);
        check_same_dimensions(&self.node_digests, &self.node_children);
        check_same_dimensions(&self.node_digests, &self.node_children_labels_ranges);
        debug_assert!(self
            .node_children_labels_ranges
            .iter()
            .all(|vec| vec.iter().all(|range| range.indexes_into(self))));
    }

    /// Returns the estimate of the size occupied by this data structure in
    /// bytes.
    pub fn size_estimate(&self) -> usize {
        fn slice_size<T>(s: &[Vec<T>]) -> usize {
            s.iter()
                .map(|vec| std::mem::size_of_val(vec) + vec.len() * std::mem::size_of::<T>())
                .sum()
        }
        std::mem::size_of_val(self)
            + slice_size(&self.leaf_digests)
            + slice_size(&self.fork_digests)
            + slice_size(&self.fork_left_children)
            + slice_size(&self.fork_right_children)
            + slice_size(&self.node_digests)
            + slice_size(&self.node_labels)
            + slice_size(&self.node_children)
            + slice_size(&self.node_children_labels_ranges)
    }

    /// Returns a structured representation-independent view of the node with
    /// the specified ID.
    pub fn view(&self, node_id: NodeId) -> HashTreeView<'_> {
        let bucket = node_id.bucket() - self.bucket_offset;
        let idx = node_id.index();
        match node_id.kind() {
            NodeKind::Fork => HashTreeView::Fork(
                &self.fork_digests[bucket][idx],
                self.fork_left_children[bucket][idx],
                self.fork_right_children[bucket][idx],
            ),
            NodeKind::Node => HashTreeView::Node(
                &self.node_digests[bucket][idx],
                &self.node_labels[bucket][idx],
                self.node_children[bucket][idx],
            ),
            NodeKind::Leaf => HashTreeView::Leaf(&self.leaf_digests[bucket][idx]),
            NodeKind::Empty => HashTreeView::Empty,
        }
    }

    /// Returns the root hash of the tree.
    pub fn root_hash(&self) -> &Digest {
        self.digest(self.root)
    }

    /// Constructs a witness for the specified partial tree.
    ///
    /// # Panics
    ///
    /// Panics if the partial tree a structure that is different from the
    /// labeled tree that was used to construct this hash tree.
    pub fn witness<B: WitnessBuilder>(&self, partial_tree: &LabeledTree<Vec<u8>>) -> B::Tree {
        fn add_forks<B: WitnessBuilder>(
            ht: &HashTree,
            pos: NodeId,
            offset: usize,
            size: usize,
            subwitness: B::Tree,
        ) -> B::Tree {
            // WARNING: FANCY DISCRETE MATH AHEAD
            //
            // The hash trees we build have a particular structure not reflected
            // in the type: all the children of forks are either forks or nodes.
            //
            // Furthermore, because of the way we build forks (pairing vertices
            // from left to right until there is only one left), they form a
            // very specific structure that is fully determined by the number of
            // nodes we had in the beginning.  Most importantly, we can compute
            // the path to K-th node without having to search it by label.
            //
            // The main observation is that if we build a subtree out of N
            // nodes, the binary representation of N can be used to determine
            // the structure of the tree we get.
            //
            // It's easy to prove by induction that if
            //
            //   N = 2^k_1 + 2^k_2 + ... + 2^k^m  (k_1 > k_2 > ... k_m)
            //
            // where k_i corresponds to the position of i-th non-zero bit of N,
            // then the tree has a shape of _m_ full binary trees (FBTs), where
            // j-th tree has k_j leaf nodes, grouped from right to left:
            //
            //    T = FBT(k_1) ^ (FBT(k_2) ^ ( ... ^ FBT(k_m)))
            //
            // Example: if N = 7 = 0b111, we have 3 FBTs with 4, 2 and 1 leaves:
            //        ^
            //    ^      ^
            //  ^   ^   ^ `
            // 0 1 2 3 4 5 6
            //
            // So if we need to locate i-th leaf node out of N in T, we can
            // follow the following procedure locate(T, i, N):
            //
            //   locate(T, 0, 1) = T
            //   locate(T, i, N = 2^k) = if i < N/2
            //                           then locate(left(T),  i,     N/2)
            //                           else locate(right(T), i-N/2, N/2)
            //   locate(T, i, N = 2^k + M) = if i < 2^k
            //                               then locate(left(T),  i,     2^k)
            //                               else locate(right(T), i-2^k, M)
            match ht.view(pos) {
                HashTreeView::Fork(_, l, r) => {
                    if size.is_power_of_two() {
                        let h = size / 2;
                        if offset < h {
                            B::make_fork(
                                add_forks::<B>(ht, l, offset, h, subwitness),
                                B::make_pruned(ht.digest(r).clone()),
                            )
                        } else {
                            B::make_fork(
                                B::make_pruned(ht.digest(l).clone()),
                                add_forks::<B>(ht, r, offset - h, h, subwitness),
                            )
                        }
                    } else {
                        let k = 32 - (size as u32).leading_zeros();
                        let mask = 1 << (k - 1);
                        if offset < mask {
                            B::make_fork(
                                add_forks::<B>(ht, l, offset, mask, subwitness),
                                B::make_pruned(ht.digest(r).clone()),
                            )
                        } else {
                            B::make_fork(
                                B::make_pruned(ht.digest(l).clone()),
                                add_forks::<B>(ht, r, offset - mask, size - mask, subwitness),
                            )
                        }
                    }
                }
                _ => subwitness,
            }
        }

        fn child_witness<B: WitnessBuilder>(
            ht: &HashTree,
            parent: NodeId,
            pos: NodeId,
            l: &Label,
            subtree: &LabeledTree<Vec<u8>>,
        ) -> B::Tree {
            let NodeIndexRange {
                bucket,
                index_range: label_range,
            } = ht.node_labels_range(parent);
            let len = label_range.len();
            let labels = &ht.node_labels[bucket][label_range.clone()];

            match labels.binary_search(l) {
                Ok(offset) => {
                    let idx = label_range.start + offset;
                    let node_id = NodeId::node(bucket, idx);
                    let subwitness = B::make_node(
                        l.clone(),
                        go::<B>(ht, node_id, ht.node_children[bucket][idx], subtree),
                    );
                    if pos.kind() == NodeKind::Node {
                        subwitness
                    } else {
                        add_forks::<B>(ht, pos, offset, len, subwitness)
                    }
                }
                Err(offset) => {
                    let pruned_label_at = |o| {
                        add_forks::<B>(
                            ht,
                            pos,
                            o,
                            len,
                            B::make_node(
                                ht.node_labels[bucket][label_range.start + o].clone(),
                                B::make_pruned(
                                    ht.digest(ht.node_children[bucket][label_range.start + o])
                                        .clone(),
                                ),
                            ),
                        )
                    };
                    // Build a proof of absence
                    if offset == 0 {
                        // The missing child is before the first label.
                        pruned_label_at(offset)
                    } else if offset == len {
                        // The missing child is after the last label.
                        pruned_label_at(offset - 1)
                    } else {
                        // The missing child is between two present children.
                        B::merge_trees(pruned_label_at(offset - 1), pruned_label_at(offset))
                    }
                }
            }
        }

        fn go<B: WitnessBuilder>(
            ht: &HashTree,
            parent: NodeId,
            pos: NodeId,
            t: &LabeledTree<Vec<u8>>,
        ) -> B::Tree {
            match t {
                LabeledTree::Leaf(data) => {
                    if pos.kind() == NodeKind::Leaf {
                        B::make_leaf(&data[..])
                    } else {
                        panic!(
                            "inconsistent tree structure: not a leaf in the original tree, \
                             parent = {:?}, pos = {:?}, hash_tree = {:?}, labeled_tree = {:?}",
                            parent, pos, ht, t
                        );
                    }
                }
                LabeledTree::SubTree(children) => children
                    .iter()
                    .map(|(l, t)| child_witness::<B>(ht, parent, pos, l, t))
                    .fold(B::make_pruned(ht.digest(pos).clone()), B::merge_trees),
            }
        }

        go::<B>(self, NodeId::empty(), self.root, partial_tree)
    }

    fn splice_subtree(&mut self, subtree: HashTree) {
        // Leafs
        self.leaf_digests.extend(subtree.leaf_digests.into_iter());

        // Forks
        self.fork_digests.extend(subtree.fork_digests.into_iter());
        self.fork_left_children
            .extend(subtree.fork_left_children.into_iter());
        self.fork_right_children
            .extend(subtree.fork_right_children.into_iter());

        // Nodes
        self.node_digests.extend(subtree.node_digests.into_iter());
        self.node_labels.extend(subtree.node_labels.into_iter());
        self.node_children.extend(subtree.node_children.into_iter());
        self.node_children_labels_ranges
            .extend(subtree.node_children_labels_ranges.into_iter());
    }
}

/// Comparator of HashTree with the older crypto::HashTree for tests
impl PartialEq<crypto::HashTree> for HashTree {
    fn eq(&self, other: &crypto::HashTree) -> bool {
        fn eq_recursive(ht: &HashTree, ht_root: NodeId, other: &crypto::HashTree) -> bool {
            ht.digest(ht_root) == other.digest()
                && match (ht_root.kind(), other) {
                    (NodeKind::Leaf | NodeKind::Empty, crypto::HashTree::Leaf { digest: _ }) => {
                        true
                    }
                    (
                        NodeKind::Fork,
                        crypto::HashTree::Fork {
                            digest: _,
                            left_tree,
                            right_tree,
                        },
                    ) => {
                        eq_recursive(
                            ht,
                            ht.fork_left_children[ht_root.bucket()][ht_root.index()],
                            left_tree,
                        ) && eq_recursive(
                            ht,
                            ht.fork_right_children[ht_root.bucket()][ht_root.index()],
                            right_tree,
                        )
                    }
                    (
                        NodeKind::Node,
                        crypto::HashTree::Node {
                            digest: _,
                            label,
                            hash_tree,
                        },
                    ) => {
                        ht.node_labels[ht_root.bucket()][ht_root.index()] == *label
                            && eq_recursive(
                                ht,
                                ht.node_children[ht_root.bucket()][ht_root.index()],
                                hash_tree,
                            )
                    }
                    _ => false,
                }
        }

        eq_recursive(self, self.root, other)
    }
}

#[derive(Debug)]
pub enum HashTreeView<'a> {
    Empty,
    Leaf(&'a Digest),
    Fork(&'a Digest, NodeId, NodeId),
    Node(&'a Digest, &'a Label, NodeId),
}

/// Materializes the provided lazy tree and builds its hash tree that can be
/// used to produce witnesses.
pub fn hash_lazy_tree(t: &LazyTree<'_>) -> HashTree {
    struct SubtreeRoot {
        children_range: NodeIndexRange,
        root: NodeId,
    }

    // We only initalize thread pools lazily the first time we need them
    enum ParStrategy {
        Sequential,
        Concurrent,
        ConcurrentInPool(scoped_threadpool::Pool),
    }

    impl ParStrategy {
        fn pool(&mut self) -> Option<&mut scoped_threadpool::Pool> {
            match self {
                Self::Sequential => None,
                Self::Concurrent => {
                    *self = Self::ConcurrentInPool(scoped_threadpool::Pool::new(
                        NUMBER_OF_CERTIFICATION_THREADS,
                    ));
                    self.pool()
                }
                Self::ConcurrentInPool(pool) => Some(pool),
            }
        }

        fn is_concurrent(&self) -> bool {
            !matches!(self, Self::Sequential)
        }
    }

    fn go(
        t: &LazyTree<'_>,
        ht: &mut HashTree,
        parent: NodeId,
        par_strategy: &mut ParStrategy,
    ) -> NodeId {
        match t {
            LazyTree::Blob(b, None) => {
                let mut h = Hasher::for_domain("ic-hashtree-leaf");
                h.update(b);
                ht.new_leaf(h.finalize())
            }
            LazyTree::Blob(_b, Some(cached_hash)) => {
                #[cfg(debug_assertions)]
                {
                    let mut h = Hasher::for_domain("ic-hashtree-leaf");
                    h.update(_b);
                    assert_eq!(h.finalize(), Digest(*cached_hash));
                }
                ht.new_leaf(Digest(*cached_hash))
            }
            LazyTree::LazyBlob(f) => {
                let b = f();
                let mut h = Hasher::for_domain("ic-hashtree-leaf");
                h.update(&b);
                ht.new_leaf(h.finalize())
            }
            LazyTree::LazyFork(f) => {
                let num_children = f.len();
                let NodeIndexRange {
                    bucket,
                    index_range: range,
                } = ht.preallocate_nodes(num_children, parent);
                let mut nodes = Vec::with_capacity(num_children);

                // We only use multithreading if the number of children is large
                // We do not pass the thread pool down after use, so we are not spawning new threads
                // in a nested way.
                if num_children > 100 && par_strategy.is_concurrent() {
                    let thread_pool = par_strategy.pool().unwrap();
                    let bucket_offset = ht.node_children.len();
                    let threads = thread_pool.thread_count() as usize;
                    let children: Vec<_> = f.children().collect();
                    let per_thread = ((children.len() + threads - 1) / threads).max(1);
                    // Each thread produces one HashTree containing the subtrees of a set of children
                    let mut subtrees: Vec<Option<HashTree>> = vec![None; threads];
                    // Since each thread is assigned multiple children, we also produce a list of roots
                    // that need to be combined correctly for the final result
                    let mut roots: Vec<Vec<SubtreeRoot>> =
                        repeat_with(|| Vec::with_capacity(per_thread))
                            .take(threads)
                            .collect();

                    thread_pool.scoped(|scope| {
                        for (i, (children, subtree, roots)) in izip!(
                            children.chunks(per_thread),
                            subtrees.iter_mut(),
                            roots.iter_mut()
                        )
                        .enumerate()
                        {
                            scope.execute(move || {
                                // In each thread, we use a bucket offset b. All e.g fork digests
                                // produced by this thread will be in ht.fork_digests[b] in the final
                                // hash tree, so the NodeIds of the internal links need to reflect that.
                                // Note that we always add new nodes, leaves and forks to bucket 0.
                                // The bucket offset only comes into play when determining NodeIds and
                                // lookup based on NodeId.
                                let mut ht = HashTree::new_with_bucket_offset(bucket_offset + i);
                                for (_, child) in children {
                                    // Since the parent is outside of `ht`, we set the parent to NodeId::empty()
                                    // and fix the link from `root` to the parent later
                                    let root = go(
                                        child,
                                        &mut ht,
                                        NodeId::empty(),
                                        &mut ParStrategy::Sequential,
                                    );
                                    roots.push(SubtreeRoot {
                                        root,
                                        children_range: ht.root_labels_range.clone(),
                                    });
                                }
                                subtree.replace(ht);
                            });
                        }
                    });

                    // Combine all subtrees to HashTree
                    for subtree in subtrees.into_iter().flatten() {
                        ht.splice_subtree(subtree);
                    }

                    // Connect all subtree roots to their labelled nodes
                    for (i, (label, _), root) in izip!(range, children, roots.into_iter().flatten())
                    {
                        ht.node_children_labels_ranges[bucket][i] = root.children_range;
                        let mut h = Hasher::for_domain("ic-hashtree-labeled");
                        h.update(label.as_bytes());
                        h.update(ht.digest(root.root).as_bytes());
                        ht.node_digests[bucket][i] = h.finalize();
                        ht.node_children[bucket][i] = root.root;
                        ht.node_labels[bucket][i] = label;
                        nodes.push(NodeId::node(bucket, i));
                    }
                } else {
                    for (i, (label, child)) in range.zip(f.children()) {
                        let child = go(&child, ht, NodeId::node(bucket, i), par_strategy);
                        let mut h = Hasher::for_domain("ic-hashtree-labeled");
                        h.update(label.as_bytes());
                        h.update(ht.digest(child).as_bytes());
                        ht.node_digests[0][i] = h.finalize();
                        ht.node_children[0][i] = child;
                        ht.node_labels[0][i] = label;
                        nodes.push(NodeId::node(bucket, i));
                    }
                }

                if nodes.is_empty() {
                    return NodeId::empty();
                } else if nodes.len() == 1 {
                    return nodes[0];
                }

                // Build a binary tree of forks on top of the labelled nodes
                let mut next = Vec::with_capacity((nodes.len() as f64 / 2.0).ceil() as usize);
                ht.reserve_forks(nodes.len() - 1);
                loop {
                    for pair in nodes.chunks_exact(2) {
                        let mut h = Hasher::for_domain("ic-hashtree-fork");
                        h.update(ht.digest(pair[0]).as_bytes());
                        h.update(ht.digest(pair[1]).as_bytes());
                        next.push(ht.new_fork(h.finalize(), pair[0], pair[1]));
                    }
                    if nodes.len() % 2 == 1 {
                        next.push(*nodes.last().unwrap());
                    }

                    if next.len() == 1 {
                        return next[0];
                    }

                    nodes.clear();
                    std::mem::swap(&mut nodes, &mut next);
                }
            }
        }
    }
    let mut ht = HashTree::new();
    ht.root = go(t, &mut ht, NodeId::empty(), &mut ParStrategy::Concurrent);
    ht.check_invariants();
    ht
}

/// Constructs a hash tree corresponding to the specified lazy tree.
/// This function is only used for benchmarks.
pub fn crypto_hash_lazy_tree(t: &LazyTree<'_>) -> crypto::HashTree {
    use crypto::HashTree;

    fn go(t: &LazyTree<'_>) -> HashTree {
        match t {
            LazyTree::Blob(b, None) => {
                let mut h = Hasher::for_domain("ic-hashtree-leaf");
                h.update(b);
                HashTree::Leaf {
                    digest: h.finalize(),
                }
            }
            LazyTree::Blob(_, Some(h)) => HashTree::Leaf { digest: Digest(*h) },
            LazyTree::LazyBlob(f) => {
                let b = f();
                let mut h = Hasher::for_domain("ic-hashtree-leaf");
                h.update(&b);
                HashTree::Leaf {
                    digest: h.finalize(),
                }
            }
            LazyTree::LazyFork(f) => {
                let mut children = VecDeque::new();
                for label in f.labels() {
                    let child = go(&f.edge(&label).expect("missing fork tree"));
                    let mut h = Hasher::for_domain("ic-hashtree-labeled");
                    h.update(label.as_bytes());
                    h.update(child.digest().as_bytes());
                    children.push_back(HashTree::Node {
                        digest: h.finalize(),
                        label,
                        hash_tree: Box::new(child),
                    });
                }

                if children.is_empty() {
                    return HashTree::Leaf { digest: EMPTY_HASH };
                }

                let mut next = VecDeque::new();
                loop {
                    while let Some(l) = children.pop_front() {
                        if let Some(r) = children.pop_front() {
                            let mut h = Hasher::for_domain("ic-hashtree-fork");
                            h.update(l.digest().as_bytes());
                            h.update(r.digest().as_bytes());
                            next.push_back(HashTree::Fork {
                                digest: h.finalize(),
                                left_tree: Box::new(l),
                                right_tree: Box::new(r),
                            });
                        } else {
                            next.push_back(l);
                        }
                    }

                    if next.len() == 1 {
                        return next.pop_front().unwrap();
                    }
                    std::mem::swap(&mut children, &mut next);
                }
            }
        }
    }
    go(t)
}

#[cfg(test)]
mod test;
