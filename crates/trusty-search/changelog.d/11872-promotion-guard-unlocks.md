Fixed
- The live-corpus promotion guard now unlocks on drop instead of relying on close (#11872). A child process forked at the same moment held a copy of the descriptor until it ran its program, which kept the live `index.redb` locked after the guard was gone.
