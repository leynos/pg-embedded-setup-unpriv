//! Ensures Fluent localisation assets from `rstest-bdd` load on the test hosts.

use std::{cell::RefCell, env};

use color_eyre::eyre::{Result, ensure, eyre};
use rstest::fixture;
use rstest_bdd::select_localizations;
use rstest_bdd_macros::{given, scenario, then, when};
use unic_langid::langid;

#[path = "support/scenario.rs"]
mod scenario;

use scenario::expect_fixture;

#[derive(Default)]
struct LocalizedWorld {
    locale_loaded: bool,
    scenario_exercised: bool,
}

type LocalizedWorldFixture = Result<RefCell<LocalizedWorld>>;

fn borrow_world(world: &LocalizedWorldFixture) -> Result<&RefCell<LocalizedWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!("localized diagnostics fixture failed: {err}"))
}

#[fixture]
fn world() -> LocalizedWorldFixture {
    env::var("CARGO_MANIFEST_DIR")
        .map(|_| ())
        .map_err(|err| eyre!(format!("missing CARGO_MANIFEST_DIR: {err}")))?;
    Ok(RefCell::new(LocalizedWorld::default()))
}

#[given("een gelokaliseerde omgeving")]
fn given_localized_environment(world: &LocalizedWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    select_localizations(&[langid!("fr")])?;
    world_cell.borrow_mut().locale_loaded = true;
    Ok(())
}

#[when("een BDD-validatie draait")]
fn when_localized_bdd_runs(world: &LocalizedWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let locale_loaded = world_cell.borrow().locale_loaded;
    world_cell.borrow_mut().scenario_exercised = locale_loaded;
    Ok(())
}

#[then("zijn gelokaliseerde diagnostics beschikbaar")]
fn then_localized_diagnostics(world: &LocalizedWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    ensure!(
        world_ref.locale_loaded && world_ref.scenario_exercised,
        "localised diagnostics must load before the scenario assertions run",
    );
    Ok(())
}

#[scenario(path = "tests/features/localized_diagnostics.feature", index = 0)]
fn scenario_localized_diagnostics(world: LocalizedWorldFixture) {
    let _ = expect_fixture(world, "localized diagnostics world");
}
