# VHS tape file for recording pice evaluate demo GIF.
# Regenerate with: vhs docs/images/pice-evaluate-demo.tape
# Must be run from the repository root directory.

Output docs/images/pice-evaluate-demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 700
Set Height 500
Set Theme "Dracula"
Set Padding 20
Set Framerate 15

# Set up a shell function so "pice" runs the mock script, then clear
Hide
Type "pice() { bash docs/images/pice-evaluate-demo.sh; }"
Enter
Type "clear"
Enter
Show

Sleep 500ms

# Type the command visually, then execute (triggers mock via shell function)
Type "pice evaluate --background --wait .claude/plans/stack-loops.md"
Sleep 500ms
Enter

# Wait for the mock script output (~2.5s of built-in sleep delays)
Sleep 5s

# Hold on final output so viewers can read it
Sleep 4s
