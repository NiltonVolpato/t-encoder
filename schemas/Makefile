.PHONY: all clean

FBS := coprocessor.fbs
CPP_OUT := ../coprocessor/include/coprocessor_generated.h
RUST_OUT := ../portable/crates/coprocessor/src/coprocessor_generated.rs

all: $(CPP_OUT) $(RUST_OUT)

$(CPP_OUT): $(FBS)
	flatc --cpp -o ../coprocessor/include/ $(FBS)

$(RUST_OUT): $(FBS)
	flatc --rust -o ../portable/crates/coprocessor/src/ $(FBS)
	rustup run nightly rustfmt $(RUST_OUT)

clean:
	rm -f $(CPP_OUT) $(RUST_OUT)
