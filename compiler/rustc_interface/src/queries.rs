use std::any::Any;
use std::sync::Arc;

use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_codegen_ssa::{CompiledModules, CrateInfo};
use rustc_data_structures::svh::Svh;
use rustc_errors::timings::TimingSection;
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_metadata::EncodedMetadata;
use rustc_middle::dep_graph::{DepGraph, WorkProduct, WorkProductMap};
use rustc_middle::ty::TyCtxt;
use rustc_session::config::{self, OutputFilenames, OutputType};
use rustc_session::{IncrCompSession, Session};
use rustc_structures::CrateType;

use crate::diagnostics::FailedWritingFile;
use crate::passes;

pub struct Linker {
    dep_graph: DepGraph,
    output_filenames: Arc<OutputFilenames>,
    // Only present when incr. comp. is enabled.
    crate_hash: Option<Svh>,
    state: LinkerState,
}

enum LinkerState {
    /// Codegen may still be running; nothing has been written yet.
    Codegen { crate_info: CrateInfo, metadata: EncodedMetadata, ongoing_codegen: Box<dyn Any> },
    /// Codegen was joined and the output written by [`Linker::finish_and_write_rlib`].
    Written { work_products: WorkProductMap },
    /// Only while moving between the two states above.
    Taken,
}

/// Lets a value cross the `Send` bound of [`rustc_thread_pool::join`] for its first closure,
/// which is always run on the calling thread.
struct OnCallingThread<T>(T);

// SAFETY: only used to pass values to and from the first closure of `rustc_thread_pool::join`,
// which runs on the thread that calls `join`, so the value never actually changes threads.
unsafe impl<T> Send for OnCallingThread<T> {}

impl<T> OnCallingThread<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

impl Linker {
    pub fn codegen_and_build_linker(
        tcx: TyCtxt<'_>,
        codegen_backend: &dyn CodegenBackend,
    ) -> Linker {
        let (ongoing_codegen, crate_info, metadata) = passes::start_codegen(codegen_backend, tcx);

        Linker {
            dep_graph: tcx.dep_graph.clone(),
            output_filenames: Arc::clone(tcx.output_filenames(())),
            crate_hash: if tcx.sess.opts.incremental.is_some() {
                Some(tcx.crate_hash(LOCAL_CRATE))
            } else {
                None
            },
            state: LinkerState::Codegen { crate_info, metadata, ongoing_codegen },
        }
    }

    /// Joins codegen. Raises a fatal error if there were errors.
    fn join(
        sess: &Session,
        incr_comp_session: Option<&IncrCompSession>,
        codegen_backend: &dyn CodegenBackend,
        output_filenames: &OutputFilenames,
        crate_info: &CrateInfo,
        metadata: &EncodedMetadata,
        ongoing_codegen: Box<dyn Any>,
    ) -> (CompiledModules, WorkProductMap) {
        let (compiled_modules, mut work_products) = sess.time("finish_ongoing_codegen", || {
            match ongoing_codegen.downcast::<CompiledModules>() {
                // This was a check only build
                Ok(compiled_modules) => (*compiled_modules, WorkProductMap::default()),

                Err(ongoing_codegen) => codegen_backend.join_codegen(
                    ongoing_codegen,
                    sess,
                    incr_comp_session,
                    output_filenames,
                    crate_info,
                ),
            }
        });

        if sess.codegen_units().as_usize() == 1 && sess.opts.unstable_opts.time_llvm_passes {
            codegen_backend.print_pass_timings()
        }

        if sess.print_llvm_stats() {
            codegen_backend.print_statistics()
        }

        if let Some(out_path) = sess.print_llvm_stats_json() {
            let llvm_stats_json = codegen_backend.print_statistics_json();

            if !llvm_stats_json.is_empty() {
                if let Err(e) = std::fs::write(&out_path, llvm_stats_json) {
                    sess.dcx().err(format!("failed to write stats to {}: {}", out_path, e));
                }
            } else {
                sess.dcx().warn(format!(
                    "requested to print LLVM statistics to JSON file {}, but the codegen backend \
                    did not provide any statistics",
                    out_path,
                ));
            }
        }

        sess.timings.end_section(sess.dcx(), TimingSection::Codegen);

        if sess.opts.incremental.is_some()
            && let Some(path) = metadata.path()
        {
            let (id, product) = rustc_incremental::copy_cgu_workproduct_to_incr_comp_cache_dir(
                sess,
                incr_comp_session.unwrap(),
                WorkProduct::METADATA_WORKPRODUCT_CGU_NAME,
                &[(OutputType::Metadata.extension(), path)],
            );
            work_products.insert(id, product);
        }

        if let Some(guar) = sess.dcx().has_errors_or_delayed_bugs() {
            guar.raise_fatal();
        }

        (compiled_modules, work_products)
    }

    /// Writes the outputs (runs the linker, writes the rlib, ...).
    fn write_outputs(
        sess: &Session,
        codegen_backend: &dyn CodegenBackend,
        output_filenames: &OutputFilenames,
        compiled_modules: CompiledModules,
        crate_info: CrateInfo,
        metadata: EncodedMetadata,
    ) {
        // The `HostMetadata` offload pass only writes the kernel manifest.
        // Codegen was already skipped so there are no files to link.
        if sess
            .opts
            .unstable_opts
            .offload
            .iter()
            .any(|o| matches!(o, config::Offload::HostMetadata(_)))
        {
            return;
        }

        if !sess
            .opts
            .output_types
            .keys()
            .any(|&i| i == OutputType::Exe || i == OutputType::Metadata)
        {
            return;
        }

        if sess.opts.unstable_opts.no_link {
            let rlink_file = output_filenames.with_extension(config::RLINK_EXT);
            CompiledModules::serialize_rlink(
                sess,
                &rlink_file,
                &compiled_modules,
                &crate_info,
                &metadata,
                output_filenames,
            )
            .unwrap_or_else(|error| {
                sess.dcx().emit_fatal(FailedWritingFile { path: &rlink_file, error })
            });
            return;
        }

        let _timer = sess.prof.verbose_generic_activity("link_crate");
        let _timing = sess.timings.section_guard(sess.dcx(), TimingSection::Linking);
        codegen_backend.link(sess, compiled_modules, crate_info, metadata, output_filenames)
    }

    /// Calls [`TyCtxt::finish`], which saves the dep graph and the query result cache, while
    /// codegen is joined and the rlib written on the calling thread, if the crate is only an
    /// rlib and the compiler is running with multiple threads. These two steps are independent
    /// and both expensive in incremental rebuilds that reuse (nearly) all codegen units, which
    /// otherwise run one after the other. [`Linker::link`] then only finalizes the incremental
    /// compilation session.
    ///
    /// Otherwise this does nothing, and `TyCtxt::finish` and [`Linker::link`] are run one after
    /// the other as usual (for other crate types, the global context should be freed before
    /// running the linker).
    pub fn finish_and_write_rlib(&mut self, tcx: TyCtxt<'_>, codegen_backend: &dyn CodegenBackend) {
        let sess = tcx.sess;
        let Some(proof) = rustc_data_structures::sync::check_dyn_thread_safe() else { return };
        if sess.opts.incremental.is_none()
            || sess.opts.unstable_opts.no_link
            || tcx.crate_types() != [CrateType::Rlib]
            // `join` must run its first closure on this thread, see `OnCallingThread`.
            || rustc_thread_pool::current_thread_index().is_none()
        {
            return;
        }
        let LinkerState::Codegen { crate_info, metadata, ongoing_codegen } =
            std::mem::replace(&mut self.state, LinkerState::Taken)
        else {
            unreachable!()
        };
        let output_filenames = &*self.output_filenames;
        let incr_comp_session = tcx.incr_comp_session;
        let write = OnCallingThread(move || {
            let (compiled_modules, work_products) = Self::join(
                sess,
                incr_comp_session,
                codegen_backend,
                output_filenames,
                &crate_info,
                &metadata,
                ongoing_codegen,
            );
            let _timer = sess.timer("link");
            Self::write_outputs(
                sess,
                codegen_backend,
                output_filenames,
                compiled_modules,
                crate_info,
                metadata,
            );
            work_products
        });
        let finish = proof.derive(move || tcx.finish());
        let (work_products, ()) = rustc_thread_pool::join(
            move || OnCallingThread(write.into_inner()()),
            move || finish.into_inner()(),
        );
        self.state = LinkerState::Written { work_products: work_products.into_inner() };
    }

    pub fn link(
        self,
        sess: &Session,
        incr_comp_session: Option<IncrCompSession>,
        codegen_backend: &dyn CodegenBackend,
    ) {
        let Linker { dep_graph, output_filenames, crate_hash, state } = self;
        let (outputs, work_products) = match state {
            LinkerState::Codegen { crate_info, metadata, ongoing_codegen } => {
                let (compiled_modules, work_products) = Self::join(
                    sess,
                    incr_comp_session.as_ref(),
                    codegen_backend,
                    &output_filenames,
                    &crate_info,
                    &metadata,
                    ongoing_codegen,
                );
                (Some((compiled_modules, crate_info, metadata)), work_products)
            }
            LinkerState::Written { work_products } => {
                if let Some(guar) = sess.dcx().has_errors_or_delayed_bugs() {
                    guar.raise_fatal();
                }
                (None, work_products)
            }
            LinkerState::Taken => unreachable!(),
        };

        let _timer = sess.timer("link");

        sess.time("serialize_work_products", || {
            rustc_incremental::save_work_product_index(
                sess,
                incr_comp_session.as_ref(),
                &dep_graph,
                work_products,
            )
        });

        let prof = sess.prof.clone();
        prof.generic_activity("drop_dep_graph").run(move || drop(dep_graph));

        // Now that we won't touch anything in the incremental compilation directory
        // any more, we can finalize it (which involves renaming it)
        rustc_incremental::finalize_session_directory(sess, incr_comp_session, crate_hash);

        if let Some((compiled_modules, crate_info, metadata)) = outputs {
            Self::write_outputs(
                sess,
                codegen_backend,
                &output_filenames,
                compiled_modules,
                crate_info,
                metadata,
            );
        }
    }
}
