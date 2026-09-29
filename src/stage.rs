use std::borrow::Cow;
use std::marker::PhantomData;

use libafl::state::HasRand;
use libafl::{
    executors::{HasTimeout, SetTimeout},
    fuzzer::Evaluator,
    inputs::Input,
    mutators::{MutationResult, Mutator},
    stages::{Restartable, Stage},
    state::{HasCorpus, HasCurrentTestcase},
    Error,
};
use libafl_bolts::Named;

use crate::{input::HttpInput, llm::LlmAgent, mutate::AtroposMutator};

pub struct AtroposStage<E, EM, S, Z> {
    name: Cow<'static, str>,
    pub havoc: AtroposMutator,
    pub llm: LlmAgent,
    phantom: PhantomData<(E, EM, S, Z)>,
}

impl<E, EM, S, Z> AtroposStage<E, EM, S, Z> {
    pub fn new(havoc: AtroposMutator, llm: LlmAgent) -> Self {
        Self {
            name: Cow::Borrowed("atropos"),
            havoc,
            llm,
            phantom: PhantomData,
        }
    }
}

impl<E, EM, S, Z> Named for AtroposStage<E, EM, S, Z> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<E, EM, S, Z> Restartable<S> for AtroposStage<E, EM, S, Z> {
    fn should_restart(&mut self, _state: &mut S) -> Result<bool, Error> {
        Ok(true)
    }

    fn clear_progress(&mut self, _state: &mut S) -> Result<(), Error> {
        Ok(())
    }
}

impl<E, EM, S, Z> Stage<E, EM, S, Z> for AtroposStage<E, EM, S, Z>
where
    E: HasTimeout + SetTimeout,
    S: HasCorpus<HttpInput> + HasCurrentTestcase<HttpInput> + HasRand,
    Z: Evaluator<E, EM, HttpInput, S> + libafl::fuzzer::ExecutesInput<E, EM, HttpInput, S>,
    HttpInput: Input,
{
    fn perform(
        &mut self,
        fuzzer: &mut Z,
        executor: &mut E,
        state: &mut S,
        manager: &mut EM,
    ) -> Result<(), Error> {
        if self.llm.should_fire(state) {
            eprintln!(
                "llm agent: {} executions without a new corpus entry",
                self.llm_stall()
            );
            return self
                .llm
                .generate_and_run(fuzzer, executor, state, manager, &mut self.havoc);
        }

        let mut input = state.current_input_cloned()?;
        let mutated = self.havoc.mutate(state, &mut input)?;
        if mutated == MutationResult::Skipped {
            return Ok(());
        }
        let (_, corpus_id) = fuzzer.evaluate_input(state, executor, manager, &input)?;
        self.havoc.post_exec(state, corpus_id)?;
        self.llm.note(corpus_id);
        Ok(())
    }
}

impl<E, EM, S, Z> AtroposStage<E, EM, S, Z> {
    fn llm_stall(&self) -> u64 {
        self.llm.execs_since_novel()
    }
}
