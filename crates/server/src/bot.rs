use graphwar_game_core::{Expr, GameState, Team, Terrain, TrajectoryMode, parse, trace};
use graphwar_protocol::GameMode;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::time::{Duration, Instant};

const POPULATION_SIZE: usize = 50;
const ELITE_COUNT: usize = 5;
const MUTATED_COUNT: usize = 25;
const MAX_GENERATIONS: usize = 8;
const MAX_GENE_TOKENS: usize = 96;
const MAX_EXPRESSION_NODES: usize = 48;
const MAX_EXPRESSION_DEPTH: usize = 8;
const ENEMY_HIT_SCORE: f64 = 2_000_000.0;
const FRIENDLY_HIT_SCORE: f64 = -2_000_000.0;
const DISTANCE_SCORE: f64 = 1_000_000.0;

#[derive(Clone, Debug, PartialEq)]
enum GeneToken {
    Number(f64),
    X,
    Y,
    Dy,
    Add,
    Mul,
    Div,
    Pow,
    Sqrt,
    Log10,
    Abs,
    Sin,
    Cos,
    Tan,
    Ln,
}

impl GeneToken {
    fn arity(&self) -> i32 {
        match self {
            Self::Number(_) | Self::X | Self::Y | Self::Dy => 0,
            Self::Add | Self::Mul | Self::Div | Self::Pow => 2,
            Self::Sqrt | Self::Log10 | Self::Abs | Self::Sin | Self::Cos | Self::Tan | Self::Ln => {
                1
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    gene: Vec<GeneToken>,
    expression: Expr,
    function: String,
    angle: f64,
    score: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchMemory {
    populations: Vec<Vec<Candidate>>,
}

pub struct SearchOutcome {
    pub shot: Option<(String, f64)>,
    pub memory: SearchMemory,
}

pub struct SearchInput<'a> {
    pub mode: GameMode,
    pub terrain: &'a Terrain,
    pub state: &'a GameState,
    pub team: Team,
    pub level: u8,
    pub seed: u64,
    pub memory: SearchMemory,
    pub budget: Duration,
}

pub fn choose_shot(
    mode: GameMode,
    terrain: &Terrain,
    state: &GameState,
    team: Team,
    level: u8,
    seed: u64,
) -> Option<(String, f64)> {
    search(SearchInput {
        mode,
        terrain,
        state,
        team,
        level,
        seed,
        memory: SearchMemory::default(),
        budget: Duration::MAX,
    })
    .shot
}

pub fn search(input: SearchInput<'_>) -> SearchOutcome {
    let SearchInput {
        mode,
        terrain,
        state,
        team,
        level,
        seed,
        mut memory,
        budget,
    } = input;
    let generations = usize::from(level.clamp(1, MAX_GENERATIONS as u8));
    let deadline = Instant::now().checked_add(budget);
    let mut rng = StdRng::seed_from_u64(seed);
    let soldier = state
        .players
        .get(state.turn)
        .map(|player| player.current_soldier)
        .unwrap_or_default();
    if memory.populations.len() <= soldier {
        memory.populations.resize_with(soldier + 1, Vec::new);
    }
    let mut population = std::mem::take(&mut memory.populations[soldier]);
    population.iter_mut().for_each(|candidate| {
        candidate.score = f64::NEG_INFINITY;
    });
    if population.len() != POPULATION_SIZE
        || population
            .iter()
            .any(|candidate| !mode_allows(&candidate.expression, mode))
    {
        population = initial_population(mode, &mut rng);
    }
    if !evaluate_population_until(&mut population, mode, terrain, state, team, deadline) {
        population.iter_mut().for_each(|candidate| {
            candidate.score = f64::NEG_INFINITY;
        });
        memory.populations[soldier] = population;
        return SearchOutcome { shot: None, memory };
    }
    sort_population(&mut population);

    for _ in 0..generations {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        let mut next = next_generation(&population, mode, &mut rng);
        if !evaluate_population_until(&mut next, mode, terrain, state, team, deadline) {
            break;
        }
        sort_population(&mut next);
        population = next;
    }

    let shot = population
        .iter()
        .find(|candidate| candidate.score.is_finite())
        .map(|candidate| (candidate.function.clone(), candidate.angle));
    memory.populations[soldier] = population;
    SearchOutcome { shot, memory }
}

fn next_generation(population: &[Candidate], mode: GameMode, rng: &mut StdRng) -> Vec<Candidate> {
    let mut next = Vec::with_capacity(POPULATION_SIZE);
    next.extend(population.iter().take(ELITE_COUNT).cloned());
    for _ in 0..MUTATED_COUNT {
        let parent = select_parent(population, rng);
        let gene = mutate(&parent.gene, mode, rng);
        let angle = mutate_angle(parent.angle, mode, rng);
        next.push(candidate(mode, gene, angle).unwrap_or_else(|| parent.clone()));
    }
    while next.len() < POPULATION_SIZE {
        let first = select_parent(population, rng);
        let second = select_parent(population, rng);
        let gene = crossover(&first.gene, &second.gene, mode, rng);
        let angle = if rng.random_bool(0.5) {
            first.angle
        } else {
            second.angle
        };
        let angle = mutate_angle(angle, mode, rng);
        next.push(candidate(mode, gene, angle).unwrap_or_else(|| first.clone()));
    }
    next
}

fn initial_population(mode: GameMode, rng: &mut StdRng) -> Vec<Candidate> {
    let mut population = Vec::with_capacity(POPULATION_SIZE);
    for _ in 0..POPULATION_SIZE {
        let gene = random_gene(mode, rng);
        let angle = random_angle(mode, rng);
        population.push(candidate(mode, gene, angle).unwrap_or_else(|| {
            candidate(mode, vec![GeneToken::X], angle).expect("x is valid in every mode")
        }));
    }
    population
}

fn candidate(mode: GameMode, gene: Vec<GeneToken>, angle: f64) -> Option<Candidate> {
    if !angle.is_finite() || gene.len() > MAX_GENE_TOKENS {
        return None;
    }
    let mut position = 0;
    let expression = expression_from_gene(&gene, &mut position)?;
    if position != gene.len()
        || expression_nodes(&expression) > MAX_EXPRESSION_NODES
        || expression_depth(&expression) > MAX_EXPRESSION_DEPTH
        || !mode_allows(&expression, mode)
    {
        return None;
    }
    let function = render(&expression);
    let scored_expression = parse(&function).ok()?;
    if function.len() > 256 || !mode_allows(&scored_expression, mode) {
        return None;
    }
    Some(Candidate {
        gene,
        expression: scored_expression,
        function,
        angle,
        score: f64::NEG_INFINITY,
    })
}

fn evaluate_population_until(
    population: &mut [Candidate],
    mode: GameMode,
    terrain: &Terrain,
    state: &GameState,
    team: Team,
    deadline: Option<Instant>,
) -> bool {
    let inverted = team == Team::Two;
    for candidate in population {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return false;
        }
        let trajectory_mode = match mode {
            GameMode::Function => TrajectoryMode::Function,
            GameMode::FirstOrder => TrajectoryMode::FirstOrder,
            GameMode::SecondOrder => TrajectoryMode::SecondOrder {
                angle: candidate.angle.to_radians(),
            },
        };
        candidate.score = trace(
            &candidate.expression,
            trajectory_mode,
            terrain,
            state,
            inverted,
        )
        .map(|trajectory| score(&trajectory, state, team))
        .unwrap_or(f64::NEG_INFINITY);
    }
    true
}

fn sort_population(population: &mut [Candidate]) {
    population.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.function.cmp(&right.function))
            .then_with(|| left.angle.total_cmp(&right.angle))
    });
}

fn select_parent<'a>(population: &'a [Candidate], rng: &mut StdRng) -> &'a Candidate {
    let minimum = population
        .iter()
        .filter_map(|candidate| candidate.score.is_finite().then_some(candidate.score))
        .min_by(f64::total_cmp)
        .unwrap_or(0.0);
    let weights = population.iter().map(|candidate| {
        if candidate.score.is_finite() {
            candidate.score - minimum + 1.0
        } else {
            0.0
        }
    });
    let total = weights.clone().sum::<f64>();
    if !total.is_finite() || total <= 0.0 {
        return &population[rng.random_range(0..population.len())];
    }
    let mut target = rng.random_range(0.0..total);
    for (candidate, weight) in population.iter().zip(weights) {
        if target < weight {
            return candidate;
        }
        target -= weight;
    }
    &population[population.len() - 1]
}

fn score(trajectory: &graphwar_game_core::Trajectory, state: &GameState, team: Team) -> f64 {
    let explosion = trajectory.points.last().copied();
    let will_hit = |player: usize, soldier: usize, x: f64, y: f64| {
        trajectory
            .hits
            .iter()
            .any(|hit| hit.player == player && hit.soldier == soldier)
            || explosion.is_some_and(|point| {
                (point.0 - x).hypot(point.1 - y) <= graphwar_game_core::constants::EXPLOSION_RADIUS
            })
    };
    let mut total = 0.0;
    for (player_index, player) in state.players.iter().enumerate() {
        for (soldier_index, soldier) in player.living() {
            if will_hit(player_index, soldier_index, soldier.x, soldier.y) {
                total += if player.team == team {
                    FRIENDLY_HIT_SCORE
                } else {
                    ENEMY_HIT_SCORE
                };
            }
        }
    }

    let mut nearest_enemy = DISTANCE_SCORE;
    for (player_index, player) in state.players.iter().enumerate() {
        if player.team == team {
            continue;
        }
        for (soldier_index, soldier) in player.living() {
            if will_hit(player_index, soldier_index, soldier.x, soldier.y) {
                continue;
            }
            for point in &trajectory.points {
                let dx = point.0 - soldier.x;
                if (team == Team::One && dx > 0.0) || (team == Team::Two && dx < 0.0) {
                    continue;
                }
                nearest_enemy = nearest_enemy
                    .min(dx.mul_add(dx, (point.1 - soldier.y) * (point.1 - soldier.y)));
            }
        }
    }
    total + DISTANCE_SCORE - nearest_enemy
}

fn random_gene(mode: GameMode, rng: &mut StdRng) -> Vec<GeneToken> {
    let length = gaussian_length(10.0, rng);
    repair_gene(random_tokens(mode, length, rng), mode, rng)
}

fn random_tokens(mode: GameMode, length: usize, rng: &mut StdRng) -> Vec<GeneToken> {
    (0..length).map(|_| random_token(mode, rng)).collect()
}

fn random_token(mode: GameMode, rng: &mut StdRng) -> GeneToken {
    if rng.random_bool(0.5) {
        random_value_token(mode, rng)
    } else {
        random_operator(rng)
    }
}

fn random_value_token(mode: GameMode, rng: &mut StdRng) -> GeneToken {
    if !rng.random_bool(0.5) {
        return GeneToken::Number(standard_normal(rng) * 10.2);
    }
    match mode {
        GameMode::Function => GeneToken::X,
        GameMode::FirstOrder => {
            if rng.random_bool(0.5) {
                GeneToken::X
            } else {
                GeneToken::Y
            }
        }
        GameMode::SecondOrder => match rng.random_range(0..3) {
            0 => GeneToken::X,
            1 => GeneToken::Y,
            _ => GeneToken::Dy,
        },
    }
}

fn random_operator(rng: &mut StdRng) -> GeneToken {
    match rng.random_range(0..19) {
        0 => GeneToken::Sqrt,
        1 => GeneToken::Log10,
        2 => GeneToken::Abs,
        3 => GeneToken::Sin,
        4 => GeneToken::Cos,
        5 => GeneToken::Tan,
        6 => GeneToken::Ln,
        7..=10 => GeneToken::Add,
        11..=13 => GeneToken::Mul,
        14..=16 => GeneToken::Div,
        _ => GeneToken::Pow,
    }
}

fn gaussian_length(mean: f64, rng: &mut StdRng) -> usize {
    (mean * standard_normal(rng).abs())
        .floor()
        .clamp(0.0, MAX_GENE_TOKENS as f64) as usize
}

fn standard_normal(rng: &mut StdRng) -> f64 {
    let uniform = rng.random_range(f64::MIN_POSITIVE..1.0);
    let angle = rng.random_range(0.0..std::f64::consts::TAU);
    (-2.0 * uniform.ln()).sqrt() * angle.cos()
}

fn repair_gene(tokens: Vec<GeneToken>, mode: GameMode, rng: &mut StdRng) -> Vec<GeneToken> {
    let mut repaired = Vec::with_capacity(MAX_GENE_TOKENS);
    let mut values_needed = 1_i32;
    for token in tokens {
        if values_needed == 0 || repaired.len() == MAX_GENE_TOKENS {
            break;
        }
        values_needed += token.arity() - 1;
        repaired.push(token);
    }
    while values_needed > 0 && repaired.len() < MAX_GENE_TOKENS {
        repaired.push(random_value_token(mode, rng));
        values_needed -= 1;
    }
    if values_needed != 0 {
        return vec![random_value_token(mode, rng)];
    }
    let mut position = 0;
    let valid_shape = expression_from_gene(&repaired, &mut position).is_some_and(|expression| {
        position == repaired.len()
            && expression_nodes(&expression) <= MAX_EXPRESSION_NODES
            && expression_depth(&expression) <= MAX_EXPRESSION_DEPTH
            && mode_allows(&expression, mode)
    });
    if valid_shape {
        repaired
    } else {
        vec![random_value_token(mode, rng)]
    }
}

fn mutate(gene: &[GeneToken], mode: GameMode, rng: &mut StdRng) -> Vec<GeneToken> {
    if rng.random_bool(0.5) {
        mutate_fine_tune(gene, mode, rng)
    } else {
        mutate_region(gene, mode, rng)
    }
}

fn mutate_fine_tune(gene: &[GeneToken], mode: GameMode, rng: &mut StdRng) -> Vec<GeneToken> {
    let values = gene
        .iter()
        .enumerate()
        .filter_map(|(index, token)| matches!(token, GeneToken::Number(_)).then_some(index))
        .collect::<Vec<_>>();
    if values.is_empty() {
        return random_gene(mode, rng);
    }
    let index = values[rng.random_range(0..values.len())];
    let mut mutated = gene.to_vec();
    let value = match mutated[index] {
        GeneToken::Number(_) if rng.random_bool(0.5) => standard_normal(rng) * 10.2,
        GeneToken::Number(value) => value * (standard_normal(rng) + 1.0),
        _ => unreachable!(),
    };
    mutated[index] = GeneToken::Number(value);
    repair_gene(mutated, mode, rng)
}

fn mutate_region(gene: &[GeneToken], mode: GameMode, rng: &mut StdRng) -> Vec<GeneToken> {
    let remove = gaussian_length(5.0, rng).min(gene.len());
    let insert = random_tokens(mode, gaussian_length(5.0, rng), rng);
    let start = rng.random_range(0..=gene.len() - remove);
    let mut mutated = Vec::with_capacity(gene.len() - remove + insert.len());
    mutated.extend_from_slice(&gene[..start]);
    mutated.extend(insert);
    mutated.extend_from_slice(&gene[start + remove..]);
    repair_gene(mutated, mode, rng)
}

fn crossover(
    first: &[GeneToken],
    second: &[GeneToken],
    mode: GameMode,
    rng: &mut StdRng,
) -> Vec<GeneToken> {
    let (first, second) = if rng.random_bool(0.5) {
        (second, first)
    } else {
        (first, second)
    };
    let copy_first = gaussian_length(5.0, rng).min(first.len());
    let copy_second = gaussian_length(5.0, rng).min(second.len());
    let first_start = rng.random_range(0..=first.len() - copy_first);
    let second_start = rng.random_range(0..=second.len() - copy_second);
    let mut child = Vec::with_capacity(first.len() - copy_first + copy_second);
    child.extend_from_slice(&first[..first_start]);
    child.extend_from_slice(&second[second_start..second_start + copy_second]);
    child.extend_from_slice(&first[first_start + copy_first..]);
    repair_gene(child, mode, rng)
}

fn expression_from_gene(gene: &[GeneToken], position: &mut usize) -> Option<Expr> {
    let token = gene.get(*position)?;
    *position += 1;
    Some(match token {
        GeneToken::Number(value) => Expr::Number(*value),
        GeneToken::X => Expr::X,
        GeneToken::Y => Expr::Y,
        GeneToken::Dy => Expr::Dy,
        GeneToken::Add => Expr::Add(
            Box::new(expression_from_gene(gene, position)?),
            Box::new(expression_from_gene(gene, position)?),
        ),
        GeneToken::Mul => Expr::Mul(
            Box::new(expression_from_gene(gene, position)?),
            Box::new(expression_from_gene(gene, position)?),
        ),
        GeneToken::Div => Expr::Div(
            Box::new(expression_from_gene(gene, position)?),
            Box::new(expression_from_gene(gene, position)?),
        ),
        GeneToken::Pow => Expr::Pow(
            Box::new(expression_from_gene(gene, position)?),
            Box::new(expression_from_gene(gene, position)?),
        ),
        GeneToken::Sqrt => Expr::Sqrt(Box::new(expression_from_gene(gene, position)?)),
        GeneToken::Log10 => Expr::Log10(Box::new(expression_from_gene(gene, position)?)),
        GeneToken::Abs => Expr::Abs(Box::new(expression_from_gene(gene, position)?)),
        GeneToken::Sin => Expr::Sin(Box::new(expression_from_gene(gene, position)?)),
        GeneToken::Cos => Expr::Cos(Box::new(expression_from_gene(gene, position)?)),
        GeneToken::Tan => Expr::Tan(Box::new(expression_from_gene(gene, position)?)),
        GeneToken::Ln => Expr::Ln(Box::new(expression_from_gene(gene, position)?)),
    })
}

fn mode_allows(expression: &Expr, mode: GameMode) -> bool {
    expression.variables_allowed(mode != GameMode::Function, mode == GameMode::SecondOrder)
}

fn mutate_angle(angle: f64, mode: GameMode, rng: &mut StdRng) -> f64 {
    if mode != GameMode::SecondOrder || !rng.random_bool(0.5) {
        return angle;
    }
    if rng.random_bool(0.5) {
        random_angle(mode, rng)
    } else {
        (angle + angle * (rng.random_range(0.0..1.0) - 0.5) / 5.0).clamp(-89.999, 89.999)
    }
}

fn random_angle(mode: GameMode, rng: &mut StdRng) -> f64 {
    if mode == GameMode::SecondOrder {
        rng.random_range(-90.0..90.0)
    } else {
        0.0
    }
}

fn expression_nodes(expression: &Expr) -> usize {
    match expression {
        Expr::Number(_) | Expr::X | Expr::Y | Expr::Dy => 1,
        Expr::Neg(value)
        | Expr::Sqrt(value)
        | Expr::Log10(value)
        | Expr::Ln(value)
        | Expr::Abs(value)
        | Expr::Sin(value)
        | Expr::Cos(value)
        | Expr::Tan(value) => 1 + expression_nodes(value),
        Expr::Add(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right)
        | Expr::Pow(left, right) => 1 + expression_nodes(left) + expression_nodes(right),
    }
}

fn expression_depth(expression: &Expr) -> usize {
    match expression {
        Expr::Number(_) | Expr::X | Expr::Y | Expr::Dy => 1,
        Expr::Neg(value)
        | Expr::Sqrt(value)
        | Expr::Log10(value)
        | Expr::Ln(value)
        | Expr::Abs(value)
        | Expr::Sin(value)
        | Expr::Cos(value)
        | Expr::Tan(value) => 1 + expression_depth(value),
        Expr::Add(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right)
        | Expr::Pow(left, right) => 1 + expression_depth(left).max(expression_depth(right)),
    }
}

fn render(expression: &Expr) -> String {
    match expression {
        Expr::Number(value) => {
            let value = if value.abs() < 0.005 { 0.0 } else { *value };
            format!("{value:.2}")
        }
        Expr::X => "x".into(),
        Expr::Y => "y".into(),
        Expr::Dy => "y'".into(),
        Expr::Neg(value) => format!("(-({}))", render(value)),
        Expr::Add(left, right) => format!("({}+{})", render(left), render(right)),
        Expr::Mul(left, right) => format!("({}*{})", render(left), render(right)),
        Expr::Div(left, right) => format!("({}/{})", render(left), render(right)),
        Expr::Pow(left, right) => format!("({}^{})", render(left), render(right)),
        Expr::Sqrt(value) => format!("sqrt({})", render(value)),
        Expr::Log10(value) => format!("log({})", render(value)),
        Expr::Ln(value) => format!("ln({})", render(value)),
        Expr::Abs(value) => format!("abs({})", render(value)),
        Expr::Sin(value) => format!("sin({})", render(value)),
        Expr::Cos(value) => format!("cos({})", render(value)),
        Expr::Tan(value) => format!("tan({})", render(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphwar_game_core::{Player, Soldier};

    fn state() -> GameState {
        GameState::new(vec![
            Player::new(1, Team::One, vec![Soldier::new(100.0, 225.0)]),
            Player::new(2, Team::Two, vec![Soldier::new(650.0, 225.0)]),
        ])
    }

    #[test]
    fn candidate_scores_the_exact_rendered_expression() {
        let candidate = candidate(
            GameMode::Function,
            vec![
                GeneToken::Div,
                GeneToken::Number(1.0),
                GeneToken::Number(0.004),
            ],
            0.0,
        )
        .unwrap();
        assert_eq!(candidate.function, "(1.00/0.00)");
        assert!(
            trace(
                &candidate.expression,
                TrajectoryMode::Function,
                &Terrain::default(),
                &state(),
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn chooser_returns_deterministic_mode_valid_shots() {
        for mode in [
            GameMode::Function,
            GameMode::FirstOrder,
            GameMode::SecondOrder,
        ] {
            let first = choose_shot(mode, &Terrain::default(), &state(), Team::One, 2, 7).unwrap();
            let second = choose_shot(mode, &Terrain::default(), &state(), Team::One, 2, 7).unwrap();
            assert_eq!(first, second);
            assert!((-90.0..=90.0).contains(&first.1));
            let expression = parse(&first.0).unwrap();
            assert!(mode_allows(&expression, mode));
            if mode != GameMode::SecondOrder {
                assert_eq!(first.1, 0.0);
            }
        }
    }

    #[test]
    fn mutation_and_crossover_stay_parseable_and_bounded() {
        let mut rng = StdRng::seed_from_u64(11);
        for mode in [
            GameMode::Function,
            GameMode::FirstOrder,
            GameMode::SecondOrder,
        ] {
            let mut first = random_gene(mode, &mut rng);
            let second = random_gene(mode, &mut rng);
            for _ in 0..100 {
                first = mutate(&first, mode, &mut rng);
                let child = crossover(&first, &second, mode, &mut rng);
                assert_valid_gene(&child, mode);
                let candidate = candidate(mode, child, random_angle(mode, &mut rng));
                assert!(candidate.is_some());
            }
        }
    }

    #[test]
    fn repaired_genes_are_mode_valid_and_deterministic() {
        for mode in [
            GameMode::Function,
            GameMode::FirstOrder,
            GameMode::SecondOrder,
        ] {
            let mut first_rng = StdRng::seed_from_u64(29);
            let mut second_rng = StdRng::seed_from_u64(29);
            for _ in 0..100 {
                let first = random_gene(mode, &mut first_rng);
                let second = random_gene(mode, &mut second_rng);
                assert_gene_eq(&first, &second);
                assert_valid_gene(&first, mode);
            }
        }
    }

    #[test]
    fn population_has_legacy_generation_composition() {
        let mut rng = StdRng::seed_from_u64(31);
        let mut population = initial_population(GameMode::Function, &mut rng);
        assert!(evaluate_population_until(
            &mut population,
            GameMode::Function,
            &Terrain::default(),
            &state(),
            Team::One,
            None,
        ));
        sort_population(&mut population);
        let mut next = Vec::with_capacity(POPULATION_SIZE);
        next.extend(population.iter().take(ELITE_COUNT).cloned());
        for _ in 0..MUTATED_COUNT {
            let parent = select_parent(&population, &mut rng);
            next.push(
                candidate(
                    GameMode::Function,
                    mutate(&parent.gene, GameMode::Function, &mut rng),
                    0.0,
                )
                .unwrap(),
            );
        }
        while next.len() < POPULATION_SIZE {
            let first = select_parent(&population, &mut rng);
            let second = select_parent(&population, &mut rng);
            next.push(
                candidate(
                    GameMode::Function,
                    crossover(&first.gene, &second.gene, GameMode::Function, &mut rng),
                    0.0,
                )
                .unwrap(),
            );
        }
        assert_eq!(next.len(), POPULATION_SIZE);
        assert_eq!(next[..ELITE_COUNT].len(), ELITE_COUNT);
        assert_eq!(
            next[ELITE_COUNT..ELITE_COUNT + MUTATED_COUNT].len(),
            MUTATED_COUNT
        );
        assert_eq!(
            next[ELITE_COUNT + MUTATED_COUNT..].len(),
            POPULATION_SIZE - ELITE_COUNT - MUTATED_COUNT
        );
    }

    #[test]
    fn search_keeps_complete_population_per_soldier() {
        let terrain = Terrain::default();
        let mut first_soldier = state();
        first_soldier.players[0]
            .soldiers
            .push(Soldier::new(120.0, 225.0));
        let initial = search(SearchInput {
            mode: GameMode::Function,
            terrain: &terrain,
            state: &first_soldier,
            team: Team::One,
            level: 1,
            seed: 41,
            memory: SearchMemory::default(),
            budget: Duration::MAX,
        });
        assert_eq!(initial.memory.populations.len(), 1);
        assert_eq!(initial.memory.populations[0].len(), POPULATION_SIZE);

        let repeated = search(SearchInput {
            mode: GameMode::Function,
            terrain: &terrain,
            state: &first_soldier,
            team: Team::One,
            level: 1,
            seed: 41,
            memory: initial.memory.clone(),
            budget: Duration::ZERO,
        });
        assert_eq!(repeated.shot, None);
        assert!(
            repeated.memory.populations[0]
                .iter()
                .all(|candidate| candidate.score == f64::NEG_INFINITY)
        );

        let repeated_population = repeated.memory.populations[0].clone();
        first_soldier.players[0].current_soldier = 1;
        let second_soldier = search(SearchInput {
            mode: GameMode::Function,
            terrain: &terrain,
            state: &first_soldier,
            team: Team::One,
            level: 1,
            seed: 41,
            memory: repeated.memory,
            budget: Duration::ZERO,
        });
        assert_eq!(second_soldier.memory.populations.len(), 2);
        assert_eq!(second_soldier.memory.populations[0], repeated_population);
        assert_eq!(second_soldier.memory.populations[1].len(), POPULATION_SIZE);
    }

    #[test]
    fn zero_budget_does_not_evaluate_initial_population() {
        let terrain = Terrain::default();
        let outcome = search(SearchInput {
            mode: GameMode::Function,
            terrain: &terrain,
            state: &state(),
            team: Team::One,
            level: 1,
            seed: 43,
            memory: SearchMemory::default(),
            budget: Duration::ZERO,
        });
        assert!(outcome.shot.is_none());
        assert_eq!(outcome.memory.populations[0].len(), POPULATION_SIZE);
        assert!(
            outcome.memory.populations[0]
                .iter()
                .all(|candidate| candidate.score == f64::NEG_INFINITY)
        );
    }

    #[test]
    fn explosion_endpoint_scores_enemy_damage() {
        let state = state();
        let trajectory = graphwar_game_core::Trajectory {
            points: vec![(638.0, 225.0)],
            hits: Vec::new(),
        };
        assert!(score(&trajectory, &state, Team::One) >= ENEMY_HIT_SCORE);
    }

    #[test]
    fn zero_budget_rolls_back_to_last_complete_population() {
        let terrain = Terrain::default();
        let initial = search(SearchInput {
            mode: GameMode::Function,
            terrain: &terrain,
            state: &state(),
            team: Team::One,
            level: 1,
            seed: 47,
            memory: SearchMemory::default(),
            budget: Duration::MAX,
        });
        let resumed = search(SearchInput {
            mode: GameMode::Function,
            terrain: &terrain,
            state: &state(),
            team: Team::One,
            level: 8,
            seed: 53,
            memory: initial.memory.clone(),
            budget: Duration::ZERO,
        });
        assert!(resumed.shot.is_none());
        assert_eq!(
            resumed.memory.populations.len(),
            initial.memory.populations.len()
        );
        assert_eq!(
            resumed.memory.populations[0]
                .iter()
                .map(|candidate| (
                    &candidate.gene,
                    &candidate.expression,
                    &candidate.function,
                    candidate.angle
                ))
                .collect::<Vec<_>>(),
            initial.memory.populations[0]
                .iter()
                .map(|candidate| (
                    &candidate.gene,
                    &candidate.expression,
                    &candidate.function,
                    candidate.angle
                ))
                .collect::<Vec<_>>()
        );
        assert!(
            resumed.memory.populations[0]
                .iter()
                .all(|candidate| candidate.score == f64::NEG_INFINITY)
        );
    }

    fn assert_valid_gene(gene: &[GeneToken], mode: GameMode) {
        let mut position = 0;
        let expression = expression_from_gene(gene, &mut position).unwrap();
        assert_eq!(position, gene.len());
        assert!(mode_allows(&expression, mode));
        assert!(expression_nodes(&expression) <= MAX_EXPRESSION_NODES);
        assert!(expression_depth(&expression) <= MAX_EXPRESSION_DEPTH);
        let text = render(&expression);
        assert!(text.len() <= 256);
        assert!(parse(&text).is_ok());
    }

    fn assert_gene_eq(left: &[GeneToken], right: &[GeneToken]) {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            match (left, right) {
                (GeneToken::Number(left), GeneToken::Number(right)) => assert_eq!(left, right),
                (GeneToken::X, GeneToken::X)
                | (GeneToken::Y, GeneToken::Y)
                | (GeneToken::Dy, GeneToken::Dy)
                | (GeneToken::Add, GeneToken::Add)
                | (GeneToken::Mul, GeneToken::Mul)
                | (GeneToken::Div, GeneToken::Div)
                | (GeneToken::Pow, GeneToken::Pow)
                | (GeneToken::Sqrt, GeneToken::Sqrt)
                | (GeneToken::Log10, GeneToken::Log10)
                | (GeneToken::Abs, GeneToken::Abs)
                | (GeneToken::Sin, GeneToken::Sin)
                | (GeneToken::Cos, GeneToken::Cos)
                | (GeneToken::Tan, GeneToken::Tan)
                | (GeneToken::Ln, GeneToken::Ln) => {}
                _ => panic!("genes differ"),
            }
        }
    }
}
