#!/bin/bash
# Mock script that echoes exact pice evaluate output for GIF recording.
# This avoids needing ANTHROPIC_API_KEY / OPENAI_API_KEY for reproducible demos.

sleep 0.5

echo ""
echo "╔══════════════════════════════════════╗"
echo "║   Stack Loops Evaluation — Tier 3    ║"
echo "╠══════════════════════════════════════╣"
sleep 0.4
echo "║ ✅ infrastructure layer          9/9 ║"
echo "║   Docker + deploy seam checks pass   ║"
sleep 0.3
echo "║ ✅ database layer               10/9 ║"
echo "║   migrations and rollback verified   ║"
sleep 0.3
echo "║ ✅ api layer                     9/9 ║"
echo "║   provider contract stayed isolated  ║"
sleep 0.3
echo "║ ✅ frontend layer                9/9 ║"
echo "║   review gate approved and resumed   ║"
sleep 0.5
echo "╠══════════════════════════════════════╣"
echo "║  Background run                      ║"
echo "║  status --follow emitted terminal    ║"
echo "║  logs --follow captured progress     ║"
sleep 0.4
echo "╠══════════════════════════════════════╣"
echo "║  Overall: PASS ✅                    ║"
echo "║  All activated layers met threshold  ║"
echo "╚══════════════════════════════════════╝"
