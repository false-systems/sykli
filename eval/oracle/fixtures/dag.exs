IO.puts(~S|{"version":"1","tasks":[{"name":"lint","command":"echo lint"},{"name":"test","command":"echo test","depends_on":["lint"]},{"name":"build","command":"echo build","depends_on":["test"]}]}|)
