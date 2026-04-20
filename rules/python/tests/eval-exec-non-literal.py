# Fixture: eval-exec-non-literal
user_input = request.form["code"]

# match: identifier argument
result = eval(user_input)

# match: exec with attribute access
exec(payload.body)

# match: subscript
eval(args[0])

# no-match: literal is safe (demo/test code)
print(eval("1 + 1"))

# no-match: exec on literal
exec("print('hi')")
