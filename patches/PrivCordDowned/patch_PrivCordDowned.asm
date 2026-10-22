[BotW_PrivCordDowned_V208]
moduleMatches = 0x6267BFD0

; PrivCord co-op "downed / revive" (BotW v208 only, the RPX the multiplayer mod targets).
;
; Link's damage component has a virtual applyDamage(this, int* life) at 0x02d44dbc (this+8 = the Player actor). It
; writes the new health (quarter hearts) through r28 in two places:
;   0x02d44f4c  stw r11, 0(r28)   (the 1/4-heart-per-tick path)
;   0x02d452a4  stw r31, 0(r28)   (normal hits; the community "Infinite Hearts" cheat nops this one)
; Both are redirected through DownClamp. With DownMode = 1, a hit that would take Link to 0 leaves him at 1/4 heart
; and sets DownState = 1 ("downed"); while downed every hit is held at 1/4 heart. PrivCord (outside the game, with
; Read/WriteProcessMemory) watches DownState, revives (writes the health through DownLifePtr and clears DownState) or
; lets Link bleed out (DownMode = 2: no clamp, the next hit is lethal; vanilla death and fairies then apply).
; DownMode starts at 0 (vanilla): without PrivCord watching, nobody could revive or bleed out a downed Link, so the
; patch only acts once PrivCord arms it. The component's per-frame update (0x02d44ca4, called from vtable slot
; 0x02d44d88; it counts down the invincibility timer) records Link's actor every frame, so PrivCord can read his
; position (actor +0x204/+0x214/+0x224, big-endian floats) before he is ever hit.
; Verified live 2026-10-19 (main PC): lethal hit held at 1/4 heart, writing the health updates the HUD, mode 2 kills.
; While downed the GamePad input is emptied (DownHookInput), so a downed Link stands still until revived.
; Only our own instructions are here; no game code is copied.

.origin = codecave

DownBlock:            ; PrivCord finds this block through Cemu's log.txt ("BotW_PrivCordDowned_V208' (Codecave: ...")
DownMagic:
.int 0x50434431       ; "PCD1" (layout version 1)
DownMode:
.int 0                ; 0 off (vanilla), 1 downed instead of dying, 2 bleed out (no clamp)
DownState:
.int 0                ; 1 while downed
DownCount:
.int 0                ; times downed since boot
DownHits:
.int 0                ; damage events seen (proves the hook runs)
DownLifePtr:
.int 0                ; guest address of Link's health (int, quarter hearts), from the last hit
DownActor:
.int 0                ; the Player actor, from the last hit
DownLastLife:
.int 0                ; health the game wanted to set on the last hit (before the clamp)
DownSelf:
.int 0                ; Link's actor, refreshed every frame by DownHookTickUpdate
DownFrames:
.int 0                ; frames seen by DownHookTickUpdate (stops while paused or loading)
DownFrozen:
.int 0                ; controller reads emptied while downed (proves the input freeze runs)
DownLease:
.int 0                ; controller reads PrivCord allows to freeze; it refills this ~10x/s, so if PrivCord stops the
                      ; controls come back within about a second

; DownClamp: r3 = new health in, clamped health out. r28 = int* life, r29 = Player actor.
; Clobbers r9, r10, r12 and CR0 (all dead at both call sites).
DownClamp:
lis r12, DownBlock@ha
addi r12, r12, DownBlock@l
lwz r9, 16(r12)
addi r9, r9, 1
stw r9, 16(r12)
stw r28, 20(r12)
stw r29, 24(r12)
stw r3, 28(r12)
lwz r9, 4(r12)
cmpwi r9, 1
bne DownClampOut     ; mode 0 or 2: vanilla
lwz r10, 8(r12)
cmpwi r10, 0
bne DownHold         ; already downed: hold at 1/4 heart
cmpwi r3, 0
bgt DownClampOut     ; survives the hit: nothing to do
li r10, 1
stw r10, 8(r12)      ; DownState = 1
lwz r9, 12(r12)
addi r9, r9, 1
stw r9, 12(r12)      ; DownCount++
DownHold:
li r3, 1
DownClampOut:
blr

DownHookTick:         ; replaces 0x02d44f4c  stw r11, 0(r28)
mr r3, r11
bl DownClamp
mr r11, r3
stw r11, 0(r28)
b 0x02d44f50

DownHookHit:          ; replaces 0x02d452a4  stw r31, 0(r28); the stack copy at 8(r1) is the health the rest uses
mr r3, r31
bl DownClamp
mr r31, r3
stw r31, 8(r1)
stw r31, 0(r28)
b 0x02d452a8

DownHookTickUpdate:   ; replaces 0x02d44cec  cmpwi r31, 0 (r31 = this+8 = Link). r9/r10 are dead here.
lis r10, DownBlock@ha
addi r10, r10, DownBlock@l
stw r31, 32(r10)
lwz r9, 36(r10)
addi r9, r9, 1
stw r9, 36(r10)
cmpwi r31, 0
b 0x02d44cf0

; ---- Input freeze: a downed Link can't move or act until revived.
; The game's only VPADRead call is at 0x030d9f48 in 0x030d9f24: VPADRead(0, r31+0x14, 16, r31+0xad8), and the sample
; count is stored at r31+0xad4 by 0x030d9f4c. Each sample is a VPADStatus (0xac bytes): +0 hold, +4 trigger,
; +8 release, +0xc left stick (x, y floats), +0x14 right stick. While DownState != 0 we clear hold, trigger and both
; sticks of every sample (release is left alone so nothing stays "held" afterwards). r3 is the function's return
; value and must survive; r0 and r9-r12 are free (the epilogue reloads r0).
DownHookInput:        ; replaces 0x030d9f4c  stw r3, 0xad4(r31)
stw r3, 0xad4(r31)
lis r12, DownBlock@ha
addi r12, r12, DownBlock@l
lwz r11, 8(r12)
cmpwi r11, 0
beq DownInputOut
cmpwi r3, 0
ble DownInputOut
lwz r11, 44(r12)
cmpwi r11, 0
ble DownInputOut
addi r11, r11, -1
stw r11, 44(r12)
lwz r11, 40(r12)
addi r11, r11, 1
stw r11, 40(r12)
addi r10, r31, 0x14
mr r9, r3
li r0, 0
DownInputLoop:
stw r0, 0(r10)
stw r0, 4(r10)
stw r0, 0xc(r10)
stw r0, 0x10(r10)
stw r0, 0x14(r10)
stw r0, 0x18(r10)
addi r10, r10, 0xac
addic. r9, r9, -1
bgt DownInputLoop
DownInputOut:
b 0x030d9f50

0x030d9f4c = b DownHookInput
0x02d44cec = b DownHookTickUpdate
0x02d44f4c = b DownHookTick
0x02d452a4 = b DownHookHit
