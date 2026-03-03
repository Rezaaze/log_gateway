use log_gateway::baseline_model::BaselineModel;

fn main() {
    let model = BaselineModel::new(0.1);
    
    // Train model with very consistent AS-path lengths (low variance)
    for _ in 0..10 {
        model.update("10.0.0.0/8", 5.0); // All values exactly 5.0
    }

    // Make AS well-known
    for i in 1..=7 {
        model.record_as_seen(64512, &format!("2024-01-{:02}", i));
    }

    // Check if 100.0 is anomalous
    let is_anomalous = model.is_anomaly("10.0.0.0/8", 100.0, 3.0);
    println!("Is 100.0 anomalous? {}", is_anomalous);
    
    // Check z-score
    let z_score = model.z_score("10.0.0.0/8", 100.0);
    println!("Z-score for 100.0: {:?}", z_score);
    
    // Check if prefix is unusual
    use log_gateway::baseline_model::prefix_features::is_unusual_prefix_length;
    let is_unusual = is_unusual_prefix_length("10.0.0.0/30");
    println!("Is 10.0.0.0/30 unusual? {}", is_unusual);
    
    // Check if AS is well-known
    let is_well_known = model.as_knowledge().is_well_known(64512);
    println!("Is AS 64512 well-known? {}", is_well_known);
    
    // Compute boost
    let boost = model.compute_confidence_boost("10.0.0.0/30", 100.0, 64512);
    println!("Boost: {}", boost);
    
    // Let's also check what the variance is
    if let Some(baseline) = model.baselines.get("10.0.0.0/8") {
        println!("EMA: {}, Variance EMA: {}, Samples: {}", baseline.ema, baseline.variance_ema, baseline.sample_count);
    }
}