// E2E Fibonacci test for one-shot benchmarking
//
// Usage: `LOAM_FIB_ARG=<ARG> cargo nextest run -E 'test(<test-name>)' --nocapture --run-ignored all`
// where <ARG> is the fibonacci input
// If `LOAM_FIB_ARG` is unset, the tests will run with `DEFAULT_FIB_ARG=500`
use p3_baby_bear::BabyBear;
use p3_field::AbstractField;
use sp1_stark::baby_bear_poseidon2::BabyBearPoseidon2;
use sp1_stark::{CpuProver, MachineProver, SP1CoreOpts, StarkGenericConfig, StarkMachine};
use std::sync::Arc;
use std::time::Instant;

use lurk::{
    core::{
        eval_direct::build_lurk_toplevel_native,
        zstore::{lurk_zstore, ZPtr},
    },
    lair::{
        chipset::{Chipset, NoChip},
        execute::{QueryRecord, Shard},
        func_chip::FuncChip,
        lair_chip::{build_chip_vector, LairMachineProgram},
        toplevel::Toplevel,
        List,
    },
};

const DEFAULT_FIB_ARG: usize = 500;

fn get_fib_arg() -> usize {
    std::env::var("LOAM_FIB_ARG")
        .unwrap_or(DEFAULT_FIB_ARG.to_string())
        .parse::<usize>()
        .expect("Expected a number")
}

fn build_lurk_expr(arg: usize) -> String {
    format!(
        "(letrec ((fib
          (lambda (n)
            (if (<= n 1) n
              (+ (fib (- n 1)) (fib (- (- n 1) 1)))))))
  (fib {arg}))"
    )
}

fn setup<C: Chipset<BabyBear>>(
    arg: usize,
    toplevel: &Arc<Toplevel<BabyBear, C, NoChip>>,
) -> (
    List<BabyBear>,
    FuncChip<BabyBear, C, NoChip>,
    QueryRecord<BabyBear>,
) {
    let code = build_lurk_expr(arg);
    let zstore = &mut lurk_zstore();
    let ZPtr { tag, digest } = zstore.read(&code, &Default::default());

    let mut record = QueryRecord::new(toplevel);
    record.inject_inv_queries("hash4", toplevel, &zstore.hashes4);

    let mut full_input = [BabyBear::zero(); 24];
    full_input[0] = tag.to_field();
    full_input[8..16].copy_from_slice(&digest);

    let args: List<_> = full_input.into();
    let lurk_main = FuncChip::from_name("lurk_main", toplevel);

    (args, lurk_main, record)
}

#[ignore]
#[test]
fn fib_e2e() {
    let arg = get_fib_arg();
    let (toplevel, ..) = build_lurk_toplevel_native();
    let (args, lurk_main, mut record) = setup(arg, &toplevel);
    let start_time = Instant::now();

    toplevel
        .execute(lurk_main.func(), &args, &mut record, None)
        .unwrap();
    let config = BabyBearPoseidon2::new();
    let machine = StarkMachine::new(
        config,
        build_chip_vector(&lurk_main),
        record.expect_public_values().len(),
        true,
    );
    let (pk, _) = machine.setup(&LairMachineProgram);
    let mut challenger_p = machine.config().challenger();
    let opts = SP1CoreOpts::default();
    let shard = Shard::new(record);
    let prover = CpuProver::new(machine);
    let _machine_proof = prover
        .prove(&pk, shard, &mut challenger_p, opts)
        .expect("Failure while proving");

    let elapsed_time = start_time.elapsed().as_secs_f32();
    println!("Total time for e2e-{arg} = {:.2} s", elapsed_time);
}
