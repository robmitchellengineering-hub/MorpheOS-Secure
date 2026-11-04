# AUDIO AND PLUGINS — what "run 64-bit audio plugins" actually asks of the Core

Rob, 2026-11-04: *"airmatrix needs to be able to run 64bit audio plugins"*. This document states what that means,
what it costs, and what the first falsifier is — before any of it is built.

## What a plugin is, stated plainly

A 64-bit audio plugin (VST3, CLAP, AU) is **a native shared library with an entry point** — not a document, not a
script. On the Pi 5 that means an **AArch64 ELF64** file produced by someone else's compiler, from someone else's
source, that expects to be `dlopen`ed into a host process and called on a real-time thread.

So "run 64-bit plugins" is three requirements, and only the third is about audio:

1. **Load and link foreign code.** An ELF64 loader, a dynamic linker, and enough of a C/C++ runtime for real plugins
   (the VST3 SDK and most plugin frameworks assume libc, libstdc++, and pthreads — not a toy ABI).
2. **Run it untrusted.** A plugin is arbitrary code with a vendor's bugs and, potentially, a vendor's malice. It must
   not be able to read the user's files, open a socket, or phone home — and it must not be able to take the machine
   down when it segfaults or spins.
3. **Feed it audio in real time.** A graph of plugins with buffers handed between them on a deadline, with no
   copying that costs the deadline, and no scheduler hiccup that drops a buffer.

## The design this implies, and why it is not a betrayal of the plan

**The plugin runs in a box: untrusted code in an isolated domain that holds only the capabilities it was handed —
its audio buffers, its own state, its parameters.** It gets no filesystem, no network, no other task's memory. If it
crashes, the box dies and the machine keeps playing; a failed plugin is not a failed session.

That box needs a **Linux user-space ABI inside it** — the same way doc 05's compatibility capsules host Windows and
Android applications. Linux is not the operating system here; a Linux-shaped *interpreter of foreign binaries* is a
compatibility device, exactly like the driver boxes. Writing it is the work.

What the Core itself must provide for this, and nothing more:

| the Core provides | the box provides |
|---|---|
| capability-granted shared buffers (audio in/out) | ELF64 loading, dynamic linking, libc/libstdc++ surface |
| a task with a real-time class, and a deadline it is held to | pthreads, atomics, `dlopen`, the plugin's own SDK glue |
| a way to kill the box and reclaim everything it held | the host-facing VST3/CLAP entry points |
| the ledger entry for the box's state (so a crash is undoable) | nothing: the box has no authority to keep state anywhere else |

**64-bit, specifically.** The Pi 5 is 64-bit-only, so "64-bit" is not a choice — `aarch64-unknown-linux-gnu`
binaries. And the audio path carries **64-bit floats** end to end for the AirMatrix DSP, because the measurement
work needs the precision; a plugin that only speaks 32-bit float is converted at the boundary, in the box, and the
conversion is not counted as part of the box's guarantee.

## The rungs this adds

| rung | what it proves | falsifier |
|---|---|---|
| **M6a** | a self-contained AArch64 ELF64 binary loads and runs in a box, with no libc of the machine's | the box runs it and the binary prints what it computed |
| **M6b** | the box's authority is only what it was handed | the same binary attempts to open a file, open a socket, and read another task's memory — all three refused, and the refusal named |
| **M6c** | a crashing or spinning box does not take the machine with it | kill it mid-buffer: the audio graph keeps producing, the box is reclaimed, the event is in the ledger |
| **M6d** | a real VST3/CLAP plugin, built for aarch64, processes a buffer | the plugin's own output is compared against the same plugin run on a normal Linux machine — sample-identical within the plugin's own tolerance |

**M6d is the claim that matters to Rob**, and it cannot be faked: the comparison is against the plugin's behaviour
on a machine that is not ours, and a difference is a finding, not a rounding error.

## What must be decided before M6a is written

1. **Which plugin formats first** — VST3, CLAP, or both. CLAP is a plain C ABI and dramatically cheaper to host;
   VST3 is what most vendors ship. This is a product decision, not an engineering preference.
2. **Whether the box may ever touch the network** (plugin licence checks, online activation). The default here is
   **no**, and a plugin that requires it is a plugin that does not run on this machine — unless Rob decides
   otherwise, in which case it is a named, logged, revocable capability rather than a policy.

## Why this is stated now rather than discovered later

Because "run 64-bit audio plugins" silently implies a dynamic linker, a C++ runtime and a sandbox, and each of those
is a project. A plan that says "plugins" and means "a compatibility box with a Linux ABI" is honest; a plan that
says "plugins" and means "`dlopen` on the Core" would be a plan for a crash.
