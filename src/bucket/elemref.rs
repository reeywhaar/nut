use either::Either;
use std::ops::Deref;

use crate::consts::Flags;
use crate::node::Node;
use crate::page::Page;

#[derive(Clone, Debug)]
pub(crate) struct PageNode(Either<*const Page, Node>);

impl Deref for PageNode {
    type Target = Either<*const Page, Node>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<*const Page> for PageNode {
    fn from(p: *const Page) -> Self {
        Self(Either::Left(p))
    }
}

impl From<Node> for PageNode {
    fn from(n: Node) -> Self {
        Self(Either::Right(n))
    }
}

impl PageNode {
    pub(crate) fn upgrade(&self) -> Either<&Page, &Node> {
        match self.0 {
            Either::Left(p) => unsafe { Either::Left(&*p) },
            Either::Right(ref n) => Either::Right(n),
        }
    }

    pub(crate) fn is_leaf(&self) -> bool {
        match self.upgrade() {
            Either::Left(p) => p.flags == Flags::LEAVES,
            Either::Right(n) => n.is_leaf(),
        }
    }

    pub(crate) fn count(&self) -> usize {
        match self.upgrade() {
            Either::Left(p) => p.count as usize,
            Either::Right(n) => n.0.inodes.borrow().len(),
        }
    }
}

/// Represents a reference to an element on a given page/node.
#[derive(Clone, Debug)]
pub(crate) struct ElemRef {
    pub(crate) el: PageNode,
    pub(crate) index: usize,
}

impl Deref for ElemRef {
    type Target = PageNode;

    fn deref(&self) -> &Self::Target {
        &self.el
    }
}
