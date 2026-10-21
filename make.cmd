@echo off
rem Windows: runs the Makefile inside WSL, where geko is built and its data live.
rem From cmd: make serve. From PowerShell: .\make serve.
wsl make %*
