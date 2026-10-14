# Terminal demo for the README. Regenerate with:  make demo
# Records the real CLI; nothing is staged.

Output docs/demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 1300
Set Height 640
Set Padding 18
Set Theme "GitHub Dark"
Set TypingSpeed 30ms

Hide
Type "export PATH=/opt/ota:$PATH PS1='$ ' && clear"
Enter
Show

Type "ota-sim matrix"
Enter
Sleep 7s

Type "clear"
Enter
Type "ota-sim life --scheme naive-ab --cut 44:erase-0"
Enter
Sleep 4s

Type "clear"
Enter
Type "ota-sim life --scheme ota-safe --cut 44:erase-0"
Enter
Sleep 5s

Type "clear"
Enter
Type "ota-sim mutants"
Enter
Sleep 7s
