//! The semantic ontology: a closed vocabulary that language packs map grammar
//! node kinds into, and that metrics are written against. This is the only
//! place where a category's metric meaning is defined.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Sem {
    #[default]
    None,
    /// Named function/method definition — becomes its own measured unit.
    FnDef,
    /// Anonymous function — attributed to the enclosing unit.
    Lambda,
    /// Class/struct/interface definition.
    TypeDef,
    If,
    /// `elif` / `else if` — flat continuation of a chain, not deeper nesting.
    ElseIf,
    Else,
    /// Conditional expression (ternary).
    Ternary,
    Loop,
    /// `match`/`switch` — one decision for the whole construct.
    Match,
    /// One arm of a match/switch.
    CaseArm,
    Try,
    Catch,
    With,
    /// Boolean operator (`and`/`or`); sequences of the same operator count once
    /// for cognitive complexity, each for cyclomatic.
    BoolOp,
    /// Comprehension/guard filter clause — a decision without a block.
    Filter,
    Assert,
    /// `break`/`continue` — free within their loop.
    Jump,
    /// `goto` — a flat +1 cognitive (Sonar): the reader must find the
    /// label. Unconditional, so no cyclomatic decision.
    Goto,
    /// `await` — transparent to every complexity metric, but the fact
    /// that a call sits under one is what separates a coroutine that
    /// RUNS from one that was created and dropped.
    Await,
    Call,
    /// A type the programmer asserted rather than proved: `as`, `x.(T)`,
    /// `(T)x`, `cast(T, x)`, `@intCast`. The compiler stops checking here
    /// and starts believing.
    Cast,
    Comment,
    Import,
    /// Any identifier — α-abstracted in clone hashing so renamed copies match.
    Ident,
    NumLit,
    StrLit,
    BoolLit,
}

impl Sem {
    /// Recorded as a control event during extraction.
    pub fn is_ctrl(self) -> bool {
        use Sem::*;
        matches!(
            self,
            If | ElseIf
                | Else
                | Ternary
                | Loop
                | Match
                | CaseArm
                | Catch
                | BoolOp
                | Filter
                | Assert
                | Jump
                | Goto
        )
    }

    /// Cognitive complexity: +1 plus the current nesting penalty.
    pub fn cognitive_nested(self) -> bool {
        use Sem::*;
        matches!(self, If | Ternary | Loop | Match | Catch)
    }

    /// Cognitive complexity: flat +1, no nesting penalty.
    pub fn cognitive_flat(self) -> bool {
        matches!(self, Sem::ElseIf | Sem::Else | Sem::Filter | Sem::Goto)
    }

    /// Deepens the cognitive nesting counter for descendants.
    /// (`else`/`elif` inherit their `if`'s increment via tree containment.)
    pub fn nests_cognitive(self) -> bool {
        use Sem::*;
        matches!(self, If | Ternary | Loop | Match | Catch | Lambda)
    }

    /// Does reaching a descendant of this node depend on a condition?
    /// `Try` is absent: its body runs unconditionally, and only the
    /// handler is contingent.
    pub fn forks_control(self) -> bool {
        use Sem::*;
        matches!(
            self,
            If | ElseIf | Else | Ternary | Loop | Match | CaseArm | Catch | BoolOp | Filter
        )
    }

    /// Deepens visual block nesting for descendants.
    pub fn nests_visual(self) -> bool {
        use Sem::*;
        matches!(self, If | Loop | Match | Try | With | TypeDef)
    }

    /// Counts as a decision point for cyclomatic complexity.
    ///
    /// Assertions deliberately do NOT count: NASA Power of 10, design by
    /// contract, and TigerStyle treat them as executable invariant
    /// documentation — a virtue, measured separately as assertion density.
    pub fn cyclomatic(self) -> bool {
        use Sem::*;
        matches!(
            self,
            If | ElseIf | Ternary | Loop | Catch | CaseArm | BoolOp | Filter
        )
    }

    /// Where a grammar's own kind ids start once shifted clear of the
    /// abstracted buckets above. A kind id and a bucket both feed the
    /// same hash, so an unshifted `identifier` kind would hash as some
    /// other language's literal.
    pub const KIND_BASE: u64 = 16;

    /// Normalization bucket for clone hashing: nodes in the same bucket hash
    /// identically regardless of spelling (Type-2 clones). `None` means the
    /// node keeps its grammar identity, shifted clear of these by
    /// [`KIND_BASE`].
    pub fn clone_bucket(self) -> Option<u64> {
        match self {
            Sem::Ident => Some(1),
            Sem::NumLit => Some(2),
            Sem::StrLit => Some(3),
            Sem::BoolLit => Some(4),
            _ => None,
        }
    }
}
