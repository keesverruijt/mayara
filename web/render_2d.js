
export { render_2d };

class render_2d {

    constructor(canvas_dom, canvas_background_dom) {
        this.dom = canvas_dom;
        this.background_dom = canvas_background_dom;
        this.redrawCanvas();
        window.onresize = function () { this.redrawCanvas(); }
    }

    setSpokes(s) {
        this.spokes = s;
    }

    setRange(r) {
        this.range = r;
    }

    setRangeControl(c) {
        this.rangeControl = c;
    }
    
    setLegend(l) {
        this.legend = l;
    }

    redrawCanvas() {
        var parent = this.dom.parentNode,
            styles = getComputedStyle(parent),
            w = parseInt(styles.getPropertyValue("width"), 10),
            h = parseInt(styles.getPropertyValue("height"), 10);

        this.dom.width = w;
        this.dom.height = h;
        this.background_dom.width = w;
        this.background_dom.height = h;

        this.width = this.dom.width;
        this.height = this.dom.height;
        this.center_x = this.width / 2;
        this.center_y = this.height / 2;
        this.beam_length = Math.trunc(Math.max(this.center_x, this.center_y) * 0.9);
        this.ctx = this.dom.getContext("2d", { alpha: true });
        this.background_ctx = this.background_dom.getContext("2d");
  
        this.pattern = document.createElement('canvas');
        this.pattern.width = 2048;
        this.pattern.height = 1;
        this.pattern_ctx = this.pattern.getContext('2d');
        this.image = this.pattern_ctx.createImageData(2048, 1);
  
        this.drawRings();
    }

    drawRings() {
        this.background_ctx.setTransform(1, 0, 0, 1, 0, 0);
        this.background_ctx.clearRect(0, 0, this.width, this.height);
  
        this.background_ctx.strokeStyle = "white";
        this.background_ctx.fillStyle = "white";
        this.background_ctx.font = "bold 16px/1 Verdana, Geneva, sans-serif";
        for (let i = 0; i <= 4; i++) {
            this.background_ctx.beginPath();
            this.background_ctx.arc(this.center_x, this.center_y, i * this.beam_length / 4, 0, 2 * Math.PI);
            this.background_ctx.stroke();
            if (i > 0 && this.range && this.rangeControl) {
                let r = Math.trunc(this.range * i / 4);
                console.log("i=" + i + " range=" + this.range + " r=" + r);
                let text = (this.rangeControl.descriptions[r]) ? this.rangeControl.descriptions[r] : undefined;
                if (text === undefined) {
                    if (r % 1000 == 0) {
                        text = (r / 1000) + " km";
                    }
                    else {
                        text = r + " m";
                    }
                }
                this.background_ctx.fillText(text, this.center_x + i * this.beam_length * 1.41 / 8, this.center_y + i * this.beam_length * -1.41 / 8);
            }
        }
  
        this.background_ctx.fillStyle = "lightblue";
        this.background_ctx.fillText("MAYARA (2D CONTEXT)", 5, 20);
    }

    drawSpoke(spoke) {
        let a = 2 * Math.PI * ((spoke.angle + this.spokes * 3 / 4) % this.spokes) / this.spokes;
        let pixels_per_item = this.beam_length * 0.9 / spoke.data.length;
        if (this.range) {
            pixels_per_item = pixels_per_item * spoke.range / this.range;
        }
        let c = Math.cos(a) * pixels_per_item;
        let s = Math.sin(a) * pixels_per_item;
 
        for (let i = 0, idx = 0; i < spoke.data.length; i++, idx += 4) {
            let v = spoke.data[i];
    
            this.image.data[idx + 0] = this.legend[v][0];
            this.image.data[idx + 1] = this.legend[v][1];
            this.image.data[idx + 2] = this.legend[v][2];
            this.image.data[idx + 3] = this.legend[v][3];
        }

        this.pattern_ctx.putImageData(this.image, 0, 0);
 
        let pattern = this.ctx.createPattern(this.pattern, "repeat-x");

        let arc_angle = 2 * Math.PI / this.spokes;

        this.ctx.setTransform(c, s, -s, c, this.center_x, this.center_y);
        this.ctx.fillStyle = pattern;
        this.ctx.beginPath();
        this.ctx.moveTo(0, 0);
        this.ctx.arc(0, 0, spoke.data.length, 0, arc_angle);
        this.ctx.closePath();
        this.ctx.fill();
  
    }
}