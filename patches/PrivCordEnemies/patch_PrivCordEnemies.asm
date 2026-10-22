[BotW_PrivCordEnemies_V208]
moduleMatches = 0x6267BFD0

; PrivCord shared enemies, phases 1-2 (BotW v208 only, the RPX the multiplayer mod targets). Made by Blackz.
;
; Each actor's per-frame job (0x037a5080) calls RootAi::calc through the RootAi's vtable (+0xa4) at 0x037a513c..
; 0x037a5154. The game itself already jumps over that call to 0x037a5158 for actors tagged CalcAISkipOnPhysPause while
; physics is paused. We take the same jump for "puppets": enemies whose HashId (actor +0x408) PrivCord has put in the
; table below, because the friend's game is running that enemy and PrivCord moves this copy to match it. Physics and
; the animation player keep running; only the AI's decisions (movement, attacks, AS changes) stop.
;
; Phase 2: each entry also carries an animation request. When PrivCord bumps the entry's request number, the next
; skipped update plays the named AS (animation sequence) on the puppet through the game's own by-name AS request
; 0x0370dcf8(ASList, SafeString* name, slot 0, layer 0, 1, -1.0, -1.0), the same call the AI makes from this spot.
;
; Each table entry has a lease, spent one per skipped frame. PrivCord refills it ~20 times a second, so if PrivCord
; stops or crashes the enemy's own AI comes back within about half a second. With EnCount = 0 (the default) the patch
; does nothing. Only our own instructions are here; no game code is copied.

.origin = codecave

EnBlock:              ; PrivCord finds this block through Cemu's log.txt ("BotW_PrivCordEnemies_V208' (Codecave: ...")
EnMagic:
.int 0x50434532       ; "PCE2" (layout version 2: 48-byte entries with an AS request)
EnCount:
.int 0                ; table entries in use (0..64); 0 = vanilla
EnSkips:
.int 0                ; AI calcs skipped since boot (proves the hook runs)
EnChecks:
.int 0                ; actors looked up in a non-empty table (diagnostics)
EnPlays:
.int 0                ; AS requests played (diagnostics)
EnReserved:
.int 0
.int 0
.int 0
EnTable:              ; 64 x 48 bytes: HashId, lease, request no., done no., SafeString {char* -> name, vtable}, name[24]
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0
.int 0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0

; Replaces 0x037a513c  lwz r3, 0x390(r31)   (r31 = the actor). Every volatile register is dead here: the next
; instructions reload r3, r9, r11 and the skip target reloads r12 and r0. r31 and the float/CR state survive the call
; below (callee-saved).
EnHook:
lis r12, EnBlock@ha
addi r12, r12, EnBlock@l
lwz r11, 4(r12)
cmpwi r11, 0
beq EnRun             ; no puppets: vanilla, nothing else read
cmplwi r11, 64
bgt EnRun
lwz r0, 0x408(r31)    ; HashId (0 for actors not placed by the map)
cmpwi r0, 0
beq EnRun
lwz r9, 12(r12)
addi r9, r9, 1
stw r9, 12(r12)
addi r10, r12, 32
mtctr r11
EnLoop:
lwz r9, 0(r10)
cmpw r9, r0
beq EnFound
addi r10, r10, 48
bdnz EnLoop
EnRun:
lwz r3, 0x390(r31)
b 0x037a5140
EnFound:
lwz r9, 4(r10)
cmpwi r9, 0
ble EnRun             ; lease ran out: PrivCord stopped refreshing, let the AI run again
addi r9, r9, -1
stw r9, 4(r10)
lwz r9, 8(r12)
addi r9, r9, 1
stw r9, 8(r12)
lwz r9, 8(r10)        ; animation request number
lwz r11, 12(r10)      ; last one played
cmpw r9, r11
beq EnSkip
stw r9, 12(r10)
lwz r11, 16(r10)      ; the name must be set up
cmpwi r11, 0
beq EnSkip
lwz r3, 0x394(r31)    ; ASList
cmpwi r3, 0
beq EnSkip
stwu r1, -0x20(r1)
mflr r0
stw r0, 0x24(r1)
addi r4, r10, 16
li r5, 0
li r6, 0
li r7, 1
lis r11, 0x1000
lfs f1, 0x310c(r11)   ; -1.0 (the game's own "default blend/speed" constant)
fmr f2, f1
bl 0x0370dcf8
lwz r0, 0x24(r1)
mtlr r0
addi r1, r1, 0x20
lis r12, EnBlock@ha
addi r12, r12, EnBlock@l
lwz r9, 16(r12)
addi r9, r9, 1
stw r9, 16(r12)
EnSkip:
b 0x037a5158          ; skip RootAi::calc, exactly like the CalcAISkipOnPhysPause path

0x037a513c = b EnHook
