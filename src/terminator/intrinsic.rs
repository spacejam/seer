use rustc::mir;
use rustc::traits::Reveal;
use rustc::ty::layout::{Layout, Size, Align};
use rustc::ty::subst::Substs;
use rustc::ty::{self, Ty};

use error::{EvalError, EvalResult};
use eval_context::EvalContext;
use lvalue::{Lvalue, LvalueExtra};
use memory::{Pointer, PointerOffset};
use value::{PrimVal, PrimValKind, Value};

impl<'a, 'tcx> EvalContext<'a, 'tcx> {
    pub(super) fn call_intrinsic(
        &mut self,
        instance: ty::Instance<'tcx>,
        args: &[mir::Operand<'tcx>],
        dest: Lvalue<'tcx>,
        dest_ty: Ty<'tcx>,
        dest_layout: &'tcx Layout,
        target: mir::BasicBlock,
    ) -> EvalResult<'tcx> {
        let arg_vals: EvalResult<Vec<Value>> = args.iter()
            .map(|arg| self.eval_operand(arg))
            .collect();
        let arg_vals = arg_vals?;
        let i32 = self.tcx.types.i32;
        let isize = self.tcx.types.isize;
        let usize = self.tcx.types.usize;
        let f32 = self.tcx.types.f32;
        let f64 = self.tcx.types.f64;
        let substs = instance.substs;

        let intrinsic_name = &self.tcx.item_name(instance.def_id()).as_str()[..];
        match intrinsic_name {
            "add_with_overflow" =>
                self.intrinsic_with_overflow(mir::BinOp::Add, &args[0], &args[1], dest, dest_ty)?,

            "sub_with_overflow" =>
                self.intrinsic_with_overflow(mir::BinOp::Sub, &args[0], &args[1], dest, dest_ty)?,

            "mul_with_overflow" =>
                self.intrinsic_with_overflow(mir::BinOp::Mul, &args[0], &args[1], dest, dest_ty)?,


            "arith_offset" => {
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                let offset = self.value_to_primval(arg_vals[1], isize)?.to_i128()?;
                let new_ptr = ptr.signed_offset(offset as i64);
                self.write_primval(dest, PrimVal::Ptr(new_ptr), dest_ty)?;
            }

            "assume" => {
                let bool = self.tcx.types.bool;
                let cond = self.value_to_primval(arg_vals[0], bool)?.to_bool()?;
                if !cond { return Err(EvalError::AssumptionNotHeld); }
            }

            "atomic_load" |
            "atomic_load_relaxed" |
            "atomic_load_acq" |
            "volatile_load" => {
                let ty = instance.substs.type_at(0);
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                self.write_value(Value::ByRef(ptr), dest, ty)?;
            }

            "atomic_store" |
            "atomic_store_relaxed" |
            "atomic_store_rel" |
            "volatile_store" => {
                let ty = instance.substs.type_at(0);
                let dest = arg_vals[0].read_ptr(&self.memory)?;
                self.write_value_to_ptr(arg_vals[1], dest, ty)?;
            }

            "atomic_fence_acq" => {
                // we are inherently singlethreaded and singlecored, this is a nop
            }

            _ if intrinsic_name.starts_with("atomic_xchg") => {
                let ty = instance.substs.type_at(0);
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                let change = self.value_to_primval(arg_vals[1], ty)?;
                let old = self.read_value(ptr, ty)?;
                let old = match old {
                    Value::ByVal(val) => val,
                    Value::ByRef(_) => bug!("just read the value, can't be byref"),
                    Value::ByValPair(..) => bug!("atomic_xchg doesn't work with nonprimitives"),
                };
                self.write_primval(dest, old, ty)?;
                self.write_primval(Lvalue::from_ptr(ptr), change, ty)?;
            }

            _ if intrinsic_name.starts_with("atomic_cxchg") => {
                let ty = instance.substs.type_at(0);
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                let expect_old = self.value_to_primval(arg_vals[1], ty)?;
                let change = self.value_to_primval(arg_vals[2], ty)?;
                let old = self.read_value(ptr, ty)?;
                let old = match old {
                    Value::ByVal(val) => val,
                    Value::ByRef(_) => bug!("just read the value, can't be byref"),
                    Value::ByValPair(..) => bug!("atomic_cxchg doesn't work with nonprimitives"),
                };
                let (val, _) = self.binary_op(mir::BinOp::Eq, old, ty, expect_old, ty)?;
                let dest = self.force_allocation(dest)?.to_ptr();
                self.write_pair_to_ptr(old, val, dest, dest_ty)?;
                self.write_primval(Lvalue::from_ptr(ptr), change, ty)?;
            }

            "atomic_or" | "atomic_or_acq" | "atomic_or_rel" | "atomic_or_acqrel" | "atomic_or_relaxed" |
            "atomic_xor" | "atomic_xor_acq" | "atomic_xor_rel" | "atomic_xor_acqrel" | "atomic_xor_relaxed" |
            "atomic_and" | "atomic_and_acq" | "atomic_and_rel" | "atomic_and_acqrel" | "atomic_and_relaxed" |
            "atomic_xadd" | "atomic_xadd_acq" | "atomic_xadd_rel" | "atomic_xadd_acqrel" | "atomic_xadd_relaxed" |
            "atomic_xsub" | "atomic_xsub_acq" | "atomic_xsub_rel" | "atomic_xsub_acqrel" | "atomic_xsub_relaxed" => {
                let ty = instance.substs.type_at(0);
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                let change = self.value_to_primval(arg_vals[1], ty)?;
                let old = self.read_value(ptr, ty)?;
                let old = match old {
                    Value::ByVal(val) => val,
                    Value::ByRef(_) => bug!("just read the value, can't be byref"),
                    Value::ByValPair(..) => bug!("atomic_xadd_relaxed doesn't work with nonprimitives"),
                };
                self.write_primval(dest, old, ty)?;
                let op = match intrinsic_name.split('_').nth(1).unwrap() {
                    "or" => mir::BinOp::BitOr,
                    "xor" => mir::BinOp::BitXor,
                    "and" => mir::BinOp::BitAnd,
                    "xadd" => mir::BinOp::Add,
                    "xsub" => mir::BinOp::Sub,
                    _ => bug!(),
                };
                // FIXME: what do atomics do on overflow?
                let (val, _) = self.binary_op(op, old, ty, change, ty)?;
                self.write_primval(Lvalue::from_ptr(ptr), val, ty)?;
            },

            "breakpoint" => unimplemented!(), // halt miri

            "copy" |
            "copy_nonoverlapping" => {
                // FIXME: check whether overlapping occurs
                let elem_ty = instance.substs.type_at(0);
                let elem_size = self.type_size(elem_ty)?.expect("cannot copy unsized value");
                let elem_align = self.type_align(elem_ty)?;
                let src = arg_vals[0].read_ptr(&self.memory)?;
                let dest = arg_vals[1].read_ptr(&self.memory)?;
                let count = self.value_to_primval(arg_vals[2], usize)?.to_u64()?;
                self.memory.copy(src, dest, count * elem_size, elem_align)?;
            }

            "ctpop" |
            "cttz" |
            "ctlz" |
            "bswap" => {
                let ty = instance.substs.type_at(0);
                let num = self.value_to_primval(arg_vals[0], ty)?;
                let kind = self.ty_to_primval_kind(ty)?;
                let num = numeric_intrinsic(intrinsic_name, num, kind)?;
                self.write_primval(dest, num, ty)?;
            }

            "discriminant_value" => {
                let ty = instance.substs.type_at(0);
                let adt_ptr = arg_vals[0].read_ptr(&self.memory)?;
                let discr_val = self.read_discriminant_value(adt_ptr, ty)?;
                self.write_primval(dest, PrimVal::Bytes(discr_val), dest_ty)?;
            }

            "sinf32" | "fabsf32" | "cosf32" |
            "sqrtf32" | "expf32" | "exp2f32" |
            "logf32" | "log10f32" | "log2f32" |
            "floorf32" | "ceilf32" | "truncf32" => {
                let f = self.value_to_primval(arg_vals[0], f32)?.to_f32()?;
                let f = match intrinsic_name {
                    "sinf32" => f.sin(),
                    "fabsf32" => f.abs(),
                    "cosf32" => f.cos(),
                    "sqrtf32" => f.sqrt(),
                    "expf32" => f.exp(),
                    "exp2f32" => f.exp2(),
                    "logf32" => f.ln(),
                    "log10f32" => f.log10(),
                    "log2f32" => f.log2(),
                    "floorf32" => f.floor(),
                    "ceilf32" => f.ceil(),
                    "truncf32" => f.trunc(),
                    _ => bug!(),
                };
                self.write_primval(dest, PrimVal::from_f32(f), dest_ty)?;
            }

            "sinf64" | "fabsf64" | "cosf64" |
            "sqrtf64" | "expf64" | "exp2f64" |
            "logf64" | "log10f64" | "log2f64" |
            "floorf64" | "ceilf64" | "truncf64" => {
                let f = self.value_to_primval(arg_vals[0], f64)?.to_f64()?;
                let f = match intrinsic_name {
                    "sinf64" => f.sin(),
                    "fabsf64" => f.abs(),
                    "cosf64" => f.cos(),
                    "sqrtf64" => f.sqrt(),
                    "expf64" => f.exp(),
                    "exp2f64" => f.exp2(),
                    "logf64" => f.ln(),
                    "log10f64" => f.log10(),
                    "log2f64" => f.log2(),
                    "floorf64" => f.floor(),
                    "ceilf64" => f.ceil(),
                    "truncf64" => f.trunc(),
                    _ => bug!(),
                };
                self.write_primval(dest, PrimVal::from_f64(f), dest_ty)?;
            }

            "fadd_fast" | "fsub_fast" | "fmul_fast" | "fdiv_fast" | "frem_fast" => {
                let ty = instance.substs.type_at(0);
                let a = self.value_to_primval(arg_vals[0], ty)?;
                let b = self.value_to_primval(arg_vals[1], ty)?;
                let op = match intrinsic_name {
                    "fadd_fast" => mir::BinOp::Add,
                    "fsub_fast" => mir::BinOp::Sub,
                    "fmul_fast" => mir::BinOp::Mul,
                    "fdiv_fast" => mir::BinOp::Div,
                    "frem_fast" => mir::BinOp::Rem,
                    _ => bug!(),
                };
                let result = self.binary_op(op, a, ty, b, ty)?;
                self.write_primval(dest, result.0, dest_ty)?;
            }

            "likely" |
            "unlikely" |
            "forget" => {}

            "init" => {
                let size = self.type_size(dest_ty)?.expect("cannot zero unsized value");
                let init = |this: &mut Self, val: Value| {
                    let zero_val = match val {
                        Value::ByRef(ptr) => {
                            this.memory.write_repeat(ptr, 0, size)?;
                            Value::ByRef(ptr)
                        },
                        // TODO(solson): Revisit this, it's fishy to check for Undef here.
                        Value::ByVal(PrimVal::Undef) => match this.ty_to_primval_kind(dest_ty) {
                            Ok(_) => Value::ByVal(PrimVal::Bytes(0)),
                            Err(_) => {
                                let ptr = this.alloc_ptr_with_substs(dest_ty, instance.substs)?;
                                this.memory.write_repeat(ptr, 0, size)?;
                                Value::ByRef(ptr)
                            }
                        },
                        Value::ByVal(_) => Value::ByVal(PrimVal::Bytes(0)),
                        Value::ByValPair(..) =>
                            Value::ByValPair(PrimVal::Bytes(0), PrimVal::Bytes(0)),
                    };
                    Ok(zero_val)
                };
                match dest {
                    Lvalue::Local { frame, local, field } => self.modify_local(frame, local, field.map(|(i, _)| i), init)?,
                    Lvalue::Ptr { ptr, extra: LvalueExtra::None } => self.memory.write_repeat(ptr, 0, size)?,
                    Lvalue::Ptr { .. } => bug!("init intrinsic tried to write to fat ptr target"),
                    Lvalue::Global(cid) => self.modify_global(cid, init)?,
                }
            }

            "min_align_of" => {
                let elem_ty = instance.substs.type_at(0);
                let elem_align = self.type_align(elem_ty)?;
                let align_val = PrimVal::from_u128(elem_align as u128);
                self.write_primval(dest, align_val, dest_ty)?;
            }

            "pref_align_of" => {
                let ty = instance.substs.type_at(0);
                let layout = self.type_layout(ty)?;
                let align = layout.align(&self.tcx.data_layout).pref();
                let align_val = PrimVal::from_u128(align as u128);
                self.write_primval(dest, align_val, dest_ty)?;
            }

            "move_val_init" => {
                let ty = instance.substs.type_at(0);
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                self.write_value_to_ptr(arg_vals[1], ptr, ty)?;
            }

            "needs_drop" => {
                let ty = instance.substs.type_at(0);
                let env = ty::ParamEnv::empty(Reveal::All);
                let needs_drop = ty.needs_drop(self.tcx, env);
                self.write_primval(dest, PrimVal::from_bool(needs_drop), dest_ty)?;
            }

            "offset" => {
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                let offset_primval = self.value_to_primval(arg_vals[1], isize)?;
                if offset_primval.is_concrete() && ptr.is_concrete() {
                    let offset = offset_primval.to_i128()? as i64;
                    let result_ptr = self.pointer_offset(ptr, substs.type_at(0), offset)?;
                    self.write_primval(dest, PrimVal::Ptr(result_ptr), dest_ty)?;
                } else {
                    let ptr_offset_primval = match ptr.offset {
                        PointerOffset::Concrete(n) => PrimVal::Bytes(n as u128),
                        PointerOffset::Abstract(sbytes) => PrimVal::Abstract(sbytes),
                    };
                    // need to account for the size of the type.
                    let size =
                        self.type_size(substs.type_at(0))?.expect(
                            "offset type must be sized");

                    let byte_offset = self.memory.constraints.add_binop_constraint(
                        mir::BinOp::Mul,
                        PrimVal::Bytes(size as u128),
                        offset_primval,
                        PrimValKind::U64);
                    let new_offset = self.memory.constraints.add_binop_constraint(
                        mir::BinOp::Add,
                        ptr_offset_primval,
                        byte_offset,
                        PrimValKind::U64);
                    if let PrimVal::Abstract(sbytes) = new_offset {
                        let new_ptr = Pointer::new_abstract(ptr.alloc_id, sbytes);
                        self.write_primval(dest, PrimVal::Ptr(new_ptr), dest_ty)?;
                    } else {
                        unreachable!()
                    }
                }
            }

            "overflowing_sub" => {
                self.intrinsic_overflowing(mir::BinOp::Sub, &args[0], &args[1], dest, dest_ty)?;
            }

            "overflowing_mul" => {
                self.intrinsic_overflowing(mir::BinOp::Mul, &args[0], &args[1], dest, dest_ty)?;
            }

            "overflowing_add" => {
                self.intrinsic_overflowing(mir::BinOp::Add, &args[0], &args[1], dest, dest_ty)?;
            }

            "powf32" => {
                let f = self.value_to_primval(arg_vals[0], f32)?.to_f32()?;
                let f2 = self.value_to_primval(arg_vals[1], f32)?.to_f32()?;
                self.write_primval(dest, PrimVal::from_f32(f.powf(f2)), dest_ty)?;
            }

            "powf64" => {
                let f = self.value_to_primval(arg_vals[0], f64)?.to_f64()?;
                let f2 = self.value_to_primval(arg_vals[1], f64)?.to_f64()?;
                self.write_primval(dest, PrimVal::from_f64(f.powf(f2)), dest_ty)?;
            }

            "fmaf32" => {
                let a = self.value_to_primval(arg_vals[0], f32)?.to_f32()?;
                let b = self.value_to_primval(arg_vals[1], f32)?.to_f32()?;
                let c = self.value_to_primval(arg_vals[2], f32)?.to_f32()?;
                self.write_primval(dest, PrimVal::from_f32(a * b + c), dest_ty)?;
            }

            "fmaf64" => {
                let a = self.value_to_primval(arg_vals[0], f64)?.to_f64()?;
                let b = self.value_to_primval(arg_vals[1], f64)?.to_f64()?;
                let c = self.value_to_primval(arg_vals[2], f64)?.to_f64()?;
                self.write_primval(dest, PrimVal::from_f64(a * b + c), dest_ty)?;
            }

            "powif32" => {
                let f = self.value_to_primval(arg_vals[0], f32)?.to_f32()?;
                let i = self.value_to_primval(arg_vals[1], i32)?.to_i128()?;
                self.write_primval(dest, PrimVal::from_f32(f.powi(i as i32)), dest_ty)?;
            }

            "powif64" => {
                let f = self.value_to_primval(arg_vals[0], f64)?.to_f64()?;
                let i = self.value_to_primval(arg_vals[1], i32)?.to_i128()?;
                self.write_primval(dest, PrimVal::from_f64(f.powi(i as i32)), dest_ty)?;
            }

            "size_of" => {
                let ty = instance.substs.type_at(0);
                let size =
                    self.type_size(ty)?.expect("size_of intrinsic called on unsized value") as u128;
                self.write_primval(dest, PrimVal::from_u128(size), dest_ty)?;
            }

            "size_of_val" => {
                let ty = instance.substs.type_at(0);
                let (size, _) = self.size_and_align_of_dst(ty, arg_vals[0])?;
                self.write_primval(dest, PrimVal::from_u128(size as u128), dest_ty)?;
            }

            "min_align_of_val" |
            "align_of_val" => {
                let ty = instance.substs.type_at(0);
                let (_, align) = self.size_and_align_of_dst(ty, arg_vals[0])?;
                self.write_primval(dest, PrimVal::from_u128(align as u128), dest_ty)?;
            }

            "type_name" => {
                let ty = instance.substs.type_at(0);
                let ty_name = ty.to_string();
                let s = self.str_to_value(&ty_name)?;
                self.write_value(s, dest, dest_ty)?;
            }
            "type_id" => {
                let ty = instance.substs.type_at(0);
                let n = self.tcx.type_id_hash(ty);
                self.write_primval(dest, PrimVal::Bytes(n as u128), dest_ty)?;
            }

            "transmute" => {
                let src_ty = substs.type_at(0);
                let dest_ty = substs.type_at(1);
                let src_align = self.type_align(src_ty)?;
                let dest_align = self.type_align(dest_ty)?;
                let size = self.type_size(dest_ty)?.expect("transmute() type must be sized");
                if dest_align < src_align {
                    let ptr = self.force_allocation(dest)?.to_ptr();
                    self.memory.mark_packed(ptr, size);
                    self.write_value_to_ptr(arg_vals[0], ptr, dest_ty)?;
                } else {
                    self.write_value(arg_vals[0], dest, dest_ty)?;
                }
            }

            "uninit" => {
                let size = dest_layout.size(&self.tcx.data_layout).bytes();
                let uninit = |this: &mut Self, val: Value| {
                    match val {
                        Value::ByRef(ptr) => {
                            this.memory.mark_definedness(ptr, size, false)?;
                            Ok(Value::ByRef(ptr))
                        },
                        _ => Ok(Value::ByVal(PrimVal::Undef)),
                    }
                };
                match dest {
                    Lvalue::Local { frame, local, field } => self.modify_local(frame, local, field.map(|(i, _)| i), uninit)?,
                    Lvalue::Ptr { ptr, extra: LvalueExtra::None } =>
                        self.memory.mark_definedness(ptr, size, false)?,
                    Lvalue::Ptr { .. } => bug!("uninit intrinsic tried to write to fat ptr target"),
                    Lvalue::Global(cid) => self.modify_global(cid, uninit)?,
                }
            }

            "write_bytes" => {
                let u8 = self.tcx.types.u8;
                let ty = instance.substs.type_at(0);
                let ty_align = self.type_align(ty)?;
                let val_byte = self.value_to_primval(arg_vals[1], u8)?.to_u128()? as u8;
                let size = self.type_size(ty)?.expect("write_bytes() type must be sized");
                let ptr = arg_vals[0].read_ptr(&self.memory)?;
                let count = self.value_to_primval(arg_vals[2], usize)?.to_u64()?;
                self.memory.check_align(ptr, ty_align, size * count)?;
                self.memory.write_repeat(ptr, val_byte, size * count)?;
            }

            name => return Err(EvalError::Unimplemented(format!("unimplemented intrinsic: {}", name))),
        }

        self.goto_block(target);

        // Since we pushed no stack frame, the main loop will act
        // as if the call just completed and it's returning to the
        // current frame.
        Ok(())
    }

    pub fn size_and_align_of_dst(
        &self,
        ty: ty::Ty<'tcx>,
        value: Value,
    ) -> EvalResult<'tcx, (u64, u64)> {
        if let Some(size) = self.type_size(ty)? {
            Ok((size as u64, self.type_align(ty)? as u64))
        } else {
            match ty.sty {
                ty::TyAdt(def, substs) => {
                    // First get the size of all statically known fields.
                    // Don't use type_of::sizing_type_of because that expects t to be sized,
                    // and it also rounds up to alignment, which we want to avoid,
                    // as the unsized field's alignment could be smaller.
                    assert!(!ty.is_simd());
                    let layout = self.type_layout(ty)?;
                    debug!("DST {} layout: {:?}", ty, layout);

                    let (sized_size, sized_align) = match *layout {
                        ty::layout::Layout::Univariant { ref variant, .. } => {
                            (variant.offsets.last().map_or(0, |o| o.bytes()), variant.align)
                        }
                        _ => {
                            bug!("size_and_align_of_dst: expcted Univariant for `{}`, found {:#?}",
                                 ty, layout);
                        }
                    };
                    debug!("DST {} statically sized prefix size: {} align: {:?}",
                           ty, sized_size, sized_align);

                    // Recurse to get the size of the dynamically sized field (must be
                    // the last field).
                    let last_field = def.struct_variant().fields.last().unwrap();
                    let field_ty = self.field_ty(substs, last_field);
                    let (unsized_size, unsized_align) = self.size_and_align_of_dst(field_ty, value)?;

                    // FIXME (#26403, #27023): We should be adding padding
                    // to `sized_size` (to accommodate the `unsized_align`
                    // required of the unsized field that follows) before
                    // summing it with `sized_size`. (Note that since #26403
                    // is unfixed, we do not yet add the necessary padding
                    // here. But this is where the add would go.)

                    // Return the sum of sizes and max of aligns.
                    let size = sized_size + unsized_size;

                    // Choose max of two known alignments (combined value must
                    // be aligned according to more restrictive of the two).
                    let align = sized_align.max(Align::from_bytes(unsized_align, unsized_align).unwrap());

                    // Issue #27023: must add any necessary padding to `size`
                    // (to make it a multiple of `align`) before returning it.
                    //
                    // Namely, the returned size should be, in C notation:
                    //
                    //   `size + ((size & (align-1)) ? align : 0)`
                    //
                    // emulated via the semi-standard fast bit trick:
                    //
                    //   `(size + (align-1)) & -align`

                    let size = Size::from_bytes(size).abi_align(align).bytes();
                    Ok((size, align.abi()))
                }
                ty::TyDynamic(..) => {
                    let (_, vtable) = value.expect_ptr_vtable_pair(&self.memory)?;
                    // the second entry in the vtable is the dynamic size of the object.
                    self.read_size_and_align_from_vtable(vtable)
                }

                ty::TySlice(_) | ty::TyStr => {
                    let elem_ty = ty.sequence_element_type(self.tcx);
                    let elem_size = self.type_size(elem_ty)?.expect("slice element must be sized") as u64;
                    let (_, len) = value.expect_slice(&self.memory)?;
                    let align = self.type_align(elem_ty)?;
                    Ok((len * elem_size, align as u64))
                }

                _ => bug!("size_of_val::<{:?}>", ty),
            }
        }
    }
    /// Returns the normalized type of a struct field
    fn field_ty(
        &self,
        param_substs: &Substs<'tcx>,
        f: &ty::FieldDef,
    ) -> ty::Ty<'tcx> {
        self.tcx.normalize_associated_type(&f.ty(self.tcx, param_substs))
    }
}

fn numeric_intrinsic<'tcx>(
    name: &str,
    val: PrimVal,
    kind: PrimValKind
) -> EvalResult<'tcx, PrimVal> {
    macro_rules! integer_intrinsic {
        ($name:expr, $val:expr, $kind:expr, $method:ident) => ({
            let val = $val;
            let bytes = val.to_bytes()?;

            use value::PrimValKind::*;
            let result_bytes = match $kind {
                I8 => (bytes as i8).$method() as u128,
                U8 => (bytes as u8).$method() as u128,
                I16 => (bytes as i16).$method() as u128,
                U16 => (bytes as u16).$method() as u128,
                I32 => (bytes as i32).$method() as u128,
                U32 => (bytes as u32).$method() as u128,
                I64 => (bytes as i64).$method() as u128,
                U64 => (bytes as u64).$method() as u128,
                I128 => (bytes as i128).$method() as u128,
                U128 => bytes.$method() as u128,
                _ => bug!("invalid `{}` argument: {:?}", $name, val),
            };

            PrimVal::Bytes(result_bytes)
        });
    }

    let result_val = match name {
        "bswap" => {
            if let PrimVal::Abstract(mut sbytes) = val {
                let num_bytes = kind.num_bytes();
                for idx in 0..(num_bytes / 2) {
                    let tmp = sbytes[idx];
                    sbytes[idx] = sbytes[num_bytes - idx - 1];
                    sbytes[num_bytes - idx - 1] = tmp;
                }
                PrimVal::Abstract(sbytes)
            } else {
               integer_intrinsic!("bswap", val, kind, swap_bytes)
           }
        }
        "ctlz"  => integer_intrinsic!("ctlz",  val, kind, leading_zeros),
        "ctpop" => integer_intrinsic!("ctpop", val, kind, count_ones),
        "cttz"  => integer_intrinsic!("cttz",  val, kind, trailing_zeros),
        _       => bug!("not a numeric intrinsic: {}", name),
    };

    Ok(result_val)
}
