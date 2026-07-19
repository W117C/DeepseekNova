use chrono::{DateTime, Utc};

pub struct MutationEvent {
    pub cost: f64,
    pub timestamp: DateTime<Utc>,
}

pub struct PrefixHealthMonitor {
    pub mutations: Vec<MutationEvent>,
    pub decay_lambda: f64, // e.g., 0.05 for moderate decay over turns/time
}

impl PrefixHealthMonitor {
    pub fn new(decay_lambda: f64) -> Self {
        Self {
            mutations: Vec::new(),
            decay_lambda,
        }
    }

    pub fn record_mutation(&mut self, cost: f64) {
        self.mutations.push(MutationEvent {
            cost,
            timestamp: Utc::now(),
        });
    }

    /// Health(t) = 100 - Σ(mutation_cost * e^(-λΔt))
    pub fn current_health(&self) -> f64 {
        let now = Utc::now();
        let mut penalty = 0.0;

        for event in &self.mutations {
            let dt = (now - event.timestamp).num_minutes() as f64;
            // In an agent loop, dt could also be represented in "turns" rather than time.
            let decayed_cost = event.cost * (-self.decay_lambda * dt.max(0.0)).exp();
            penalty += decayed_cost;
        }

        (100.0 - penalty).clamp(0.0, 100.0)
    }
}
