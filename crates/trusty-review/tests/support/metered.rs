//! A metered `LlmProvider` wrapper with a hard cost cap, for the model-eval
//! harness.
//!
//! Why: `ReviewResult` carries the reviewer's cost only; the verifier's is read
//! nowhere. A live comparison spends real money on both, so the harness meters
//! every call itself and stops before the operator's cap.
//! What: [`Metered`] forwards each call to the wrapped provider, prices it
//! with `estimate_bedrock_cost_usd`, adds it to its own [`Usage`] and to a
//! shared [`Budget`]. Once the budget's total reaches its cap, every further
//! call is refused with `LlmError::Validation` before it reaches the provider.
//! Concurrent calls already in flight when the cap is reached still complete,
//! so the overshoot is bounded by the calls in flight.
//! Test: `cost_cap_refuses_the_next_call_once_reached`,
//! `meter_sums_reviewer_and_verifier_cost`.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use serde::Serialize;
use trusty_review::llm::bedrock::estimate_bedrock_cost_usd;
use trusty_review::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse, strip_provider_prefix};

/// Total spend across every metered provider of one run, and its cap.
pub struct Budget {
    cap_usd: f64,
    spent_usd: Mutex<f64>,
}

impl Budget {
    /// A budget that refuses calls once `cap_usd` is spent.
    pub fn new(cap_usd: f64) -> Arc<Self> {
        Arc::new(Self {
            cap_usd,
            spent_usd: Mutex::new(0.0),
        })
    }

    /// A budget with no cap, for offline runs that only count.
    pub fn unbounded() -> Arc<Self> {
        Self::new(f64::INFINITY)
    }

    /// USD spent so far.
    pub fn spent(&self) -> f64 {
        *self
            .spent_usd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The cap in USD.
    pub fn cap(&self) -> f64 {
        self.cap_usd
    }

    /// Whether the total has reached the cap.
    pub fn exhausted(&self) -> bool {
        self.spent() >= self.cap_usd
    }

    fn add(&self, usd: f64) {
        *self
            .spent_usd
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += usd;
    }
}

/// What one metered provider spent.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Usage {
    /// Calls that reached the provider.
    pub calls: u32,
    /// Calls refused by the cost cap.
    pub refused: u32,
    /// Calls the provider failed (throttle, transport, validation...).
    pub errors: u32,
    /// Of `errors`, those that were `LlmError::Validation`.
    pub validation_errors: u32,
    /// Calls that returned tokens but priced at $0 (no pricing entry).
    pub unpriced: u32,
    /// Input tokens over all calls.
    pub input_tokens: u64,
    /// Output tokens over all calls.
    pub output_tokens: u64,
    /// Priced cost over all calls, USD.
    pub cost_usd: f64,
    /// Provider-reported latency over all calls, ms.
    pub latency_ms: u64,
}

impl Usage {
    /// Add `other` into `self`.
    pub fn absorb(&mut self, other: &Usage) {
        self.calls += other.calls;
        self.refused += other.refused;
        self.errors += other.errors;
        self.validation_errors += other.validation_errors;
        self.unpriced += other.unpriced;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cost_usd += other.cost_usd;
        self.latency_ms += other.latency_ms;
    }
}

/// Price one call: the response's model id, or the request's when the
/// provider leaves it empty, with any routing prefix stripped.
pub fn call_cost_usd(req_model: &str, resp: &LlmResponse) -> f64 {
    let model = if resp.model.is_empty() {
        req_model
    } else {
        resp.model.as_str()
    };
    estimate_bedrock_cost_usd(
        strip_provider_prefix(model),
        resp.input_tokens,
        resp.output_tokens,
    )
}

/// An `LlmProvider` that meters and caps the provider it wraps.
pub struct Metered {
    inner: Arc<dyn LlmProvider>,
    budget: Arc<Budget>,
    usage: Mutex<Usage>,
}

impl Metered {
    /// Wrap `inner`, charging `budget`.
    pub fn new(inner: Arc<dyn LlmProvider>, budget: Arc<Budget>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            budget,
            usage: Mutex::new(Usage::default()),
        })
    }

    /// What this provider has spent so far.
    pub fn usage(&self) -> Usage {
        *self.usage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, f: impl FnOnce(&mut Usage)) {
        f(&mut self.usage.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

#[async_trait]
impl LlmProvider for Metered {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        if self.budget.exhausted() {
            self.record(|u| u.refused += 1);
            return Err(LlmError::Validation(format!(
                "model-eval cost cap reached: ${:.4} spent of ${:.2}",
                self.budget.spent(),
                self.budget.cap()
            )));
        }
        let req_model = req.model.clone();
        let resp = match self.inner.complete(req).await {
            Ok(resp) => resp,
            Err(e) => {
                let validation = matches!(e, LlmError::Validation(_));
                self.record(|u| {
                    u.errors += 1;
                    u.validation_errors += u32::from(validation);
                });
                return Err(e);
            }
        };
        let cost = call_cost_usd(&req_model, &resp);
        self.budget.add(cost);
        self.record(|u| {
            u.calls += 1;
            u.input_tokens += u64::from(resp.input_tokens);
            u.output_tokens += u64::from(resp.output_tokens);
            u.cost_usd += cost;
            u.latency_ms += resp.latency_ms;
            if cost == 0.0 && resp.input_tokens + resp.output_tokens > 0 {
                u.unpriced += 1;
            }
        });
        Ok(resp)
    }
}
