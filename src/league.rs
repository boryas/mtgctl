use clap::Args;

const ENTRY_COST: f64 = 10.0;

// Prize structure: 0W=0, 1W=0, 2W=5, 3W=12, 4W=22, 5W=37 tix
const PRIZES: [f64; 6] = [0.0, 0.0, 5.0, 12.0, 22.0, 37.0];

#[derive(Args)]
pub struct LeagueArgs {
    #[arg(short, long, help = "Win rate as a percentage (0-100)")]
    winrate: f64,
    
    #[arg(short, long, default_value = "999", help = "Number of losses before dropping (999 = never drop)")]
    drop_at_losses: usize,
    
    #[arg(short, long, default_value = "10", help = "Number of matches played per week")]
    matches_per_week: f64,
}

pub fn run(args: LeagueArgs) {
    let winrate = args.winrate / 100.0;
    
    if winrate <= 0.0 || winrate >= 1.0 {
        eprintln!("Error: Win rate must be between 0 and 100");
        return;
    }
    
    println!("=== MTGO Legacy League Trophy Analysis ===");
    println!("Win rate: {:.1}%", args.winrate);
    println!("Drop at: {} losses", if args.drop_at_losses >= 999 { "never".to_string() } else { args.drop_at_losses.to_string() });
    println!("Matches per week: {:.0}", args.matches_per_week);
    println!("Entry cost: {:.0} tix", ENTRY_COST);
    println!();
    
    let analysis = calculate_league_analysis(winrate, args.drop_at_losses, args.matches_per_week);
    
    println!("=== Results ===");
    println!("Trophy probability per league: {:.2}%", analysis.trophy_probability * 100.0);
    println!("Expected matches per league: {:.1}", analysis.expected_matches_per_league);
    println!("Expected matches per trophy: {:.1}", analysis.expected_matches_per_trophy);
    println!("Expected leagues per trophy: {:.1}", analysis.expected_leagues_per_trophy);
    println!("Expected cost per trophy: {:.1} tix", analysis.expected_cost_per_trophy);
    println!("Expected value per league: {:.2} tix", analysis.expected_value_per_league);
    println!("Expected time to trophy: {:.1} weeks", analysis.expected_weeks_to_trophy);
    println!("Break-even win rate: {:.1}%", calculate_breakeven_winrate(args.drop_at_losses) * 100.0);
}

struct LeagueAnalysis {
    trophy_probability: f64,
    expected_matches_per_league: f64,
    expected_matches_per_trophy: f64,
    expected_leagues_per_trophy: f64,
    expected_cost_per_trophy: f64,
    expected_value_per_league: f64,
    expected_weeks_to_trophy: f64,
}

fn calculate_league_analysis(winrate: f64, drop_at_losses: usize, matches_per_week: f64) -> LeagueAnalysis {
    let lossrate = 1.0 - winrate;
    
    // Trophy probability is always winrate^5
    let trophy_probability = winrate.powi(5);
    
    // Calculate expected outcomes for each league based on drop strategy
    let (expected_matches_per_league, expected_net_cost_per_league) = if drop_at_losses >= 999 {
        // Never drop - always play all 5 matches
        let expected_matches = 5.0;
        let mut expected_prizes = 0.0;
        
        for wins in 0..=5 {
            let losses = 5 - wins;
            let prob = binomial_coefficient(5, wins) as f64 * 
                      winrate.powi(wins as i32) * lossrate.powi(losses as i32);
            expected_prizes += PRIZES[wins] * prob;
        }
        
        (expected_matches, ENTRY_COST - expected_prizes)
    } else {
        calculate_drop_at_losses(winrate, drop_at_losses)
    };
    
    let expected_leagues_per_trophy = 1.0 / trophy_probability;
    let expected_matches_per_trophy = expected_matches_per_league * expected_leagues_per_trophy;
    let expected_cost_per_trophy = expected_net_cost_per_league * expected_leagues_per_trophy;
    let expected_value_per_league = -expected_net_cost_per_league;
    let expected_weeks_to_trophy = expected_matches_per_trophy / matches_per_week;
    
    LeagueAnalysis {
        trophy_probability,
        expected_matches_per_league,
        expected_matches_per_trophy,
        expected_leagues_per_trophy,
        expected_cost_per_trophy,
        expected_value_per_league,
        expected_weeks_to_trophy,
    }
}

fn calculate_drop_at_losses(winrate: f64, max_losses: usize) -> (f64, f64) {
    let lossrate = 1.0 - winrate;
    
    // Use recursive approach to model each match
    fn calculate_from_state(
        winrate: f64,
        lossrate: f64,
        wins: usize,
        losses: usize,
        matches_played: usize,
        max_losses: usize,
    ) -> (f64, f64) {
        // Returns (expected_matches, expected_prizes)
        
        // Check if we should drop (hit max losses)
        if losses >= max_losses {
            return (matches_played as f64, PRIZES[wins]);
        }
        
        // Check if league is complete (5 matches played)
        if matches_played >= 5 {
            return (5.0, PRIZES[wins]);
        }
        
        // Play next match
        let (matches_if_win, prizes_if_win) = calculate_from_state(
            winrate, lossrate, wins + 1, losses, matches_played + 1, max_losses
        );
        let (matches_if_loss, prizes_if_loss) = calculate_from_state(
            winrate, lossrate, wins, losses + 1, matches_played + 1, max_losses
        );
        
        let expected_matches = winrate * matches_if_win + lossrate * matches_if_loss;
        let expected_prizes = winrate * prizes_if_win + lossrate * prizes_if_loss;
        
        (expected_matches, expected_prizes)
    }
    
    let (matches, prizes) = calculate_from_state(winrate, lossrate, 0, 0, 0, max_losses);
    (matches, ENTRY_COST - prizes)
}

fn binomial_coefficient(n: usize, k: usize) -> usize {
    if k > n { return 0; }
    if k == 0 || k == n { return 1; }
    
    let mut result = 1;
    for i in 0..k {
        result = result * (n - i) / (i + 1);
    }
    result
}

fn calculate_breakeven_winrate(drop_at_losses: usize) -> f64 {
    let mut low = 0.0;
    let mut high = 1.0;
    let epsilon = 1e-6;
    
    while high - low > epsilon {
        let mid: f64 = (low + high) / 2.0;
        let analysis = calculate_league_analysis(mid, drop_at_losses, 1.0); // matches_per_week doesn't affect EV
        
        if analysis.expected_value_per_league < 0.0 {
            low = mid;
        } else {
            high = mid;
        }
    }
    
    (low + high) / 2.0
}