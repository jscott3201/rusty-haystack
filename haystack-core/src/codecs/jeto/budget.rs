/// Costs are cumulative, including freed temporaries. Input and Output report
/// document byte lengths; an embedding that already reserved raw transport
/// bytes may validate Input without reserving those same bytes a second time.
#[derive(Debug, Clone, Copy)]
pub enum Charge {
    Input(usize),
    Output(usize),
    Work(usize),
    Retained(usize),
    Nodes(usize),
    Depth(usize),
}
/// A synchronous checkpoint. Implementations may also check an absolute
/// deadline or cancellation on every call. Never renew either per codec stage.
pub trait Meter {
    type Error;
    fn charge(&mut self, cost: Charge) -> Result<(), Self::Error>;
}
impl<E, F: FnMut(Charge) -> Result<(), E>> Meter for F {
    type Error = E;
    fn charge(&mut self, cost: Charge) -> Result<(), E> {
        self(cost)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_work: usize,
    pub max_retained_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1_048_576,
            max_output_bytes: 1_048_576,
            max_work: 8_000_000,
            max_retained_bytes: 32 * 1024 * 1024,
            max_nodes: 100_000,
            max_depth: 64,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Limit {
    #[error("invalid limits")]
    Invalid,
    #[error("input bytes")]
    Input,
    #[error("output bytes")]
    Output,
    #[error("work")]
    Work,
    #[error("retained bytes")]
    Retained,
    #[error("nodes")]
    Nodes,
    #[error("depth")]
    Depth,
}
pub(super) struct Bounded {
    limits: Limits,
    work: usize,
    retained: usize,
    nodes: usize,
}
impl Bounded {
    pub(super) fn new(limits: Limits) -> Result<Self, Limit> {
        if limits.max_input_bytes == 0
            || limits.max_output_bytes == 0
            || limits.max_work == 0
            || limits.max_retained_bytes == 0
            || limits.max_nodes == 0
            || !(1..=64).contains(&limits.max_depth)
        {
            return Err(Limit::Invalid);
        }
        Ok(Self {
            limits,
            work: 0,
            retained: 0,
            nodes: 0,
        })
    }
}
impl Meter for Bounded {
    type Error = Limit;
    fn charge(&mut self, cost: Charge) -> Result<(), Limit> {
        let (used, amount, max, kind) = match cost {
            Charge::Input(n) => {
                return if n <= self.limits.max_input_bytes {
                    Ok(())
                } else {
                    Err(Limit::Input)
                };
            }
            Charge::Output(n) => {
                return if n <= self.limits.max_output_bytes {
                    Ok(())
                } else {
                    Err(Limit::Output)
                };
            }
            Charge::Depth(n) => {
                return if n <= self.limits.max_depth {
                    Ok(())
                } else {
                    Err(Limit::Depth)
                };
            }
            Charge::Work(n) => (&mut self.work, n, self.limits.max_work, Limit::Work),
            Charge::Retained(n) => (
                &mut self.retained,
                n,
                self.limits.max_retained_bytes,
                Limit::Retained,
            ),
            Charge::Nodes(n) => (&mut self.nodes, n, self.limits.max_nodes, Limit::Nodes),
        };
        if amount > max.saturating_sub(*used) {
            return Err(kind);
        }
        *used += amount;
        Ok(())
    }
}
